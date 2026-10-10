import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Dialog, Tabs, TabsContent, TabsList, TabsTrigger } from "@/ui";
import { useSettingsStore } from "./settingsStore";
import { AboutTab } from "./tabs/AboutTab";
import { GeneralTab } from "./tabs/GeneralTab";
import { PerformanceTab } from "./tabs/PerformanceTab";
import { StorageTab } from "./tabs/StorageTab";
import { UpdatesTab } from "./tabs/UpdatesTab";

export type SettingsTab = "general" | "performance" | "updates" | "storage" | "about";

const TABS: readonly { value: SettingsTab; labelKey: string }[] = [
  { value: "general", labelKey: "settings.tabs.general" },
  { value: "performance", labelKey: "settings.tabs.performance" },
  { value: "updates", labelKey: "settings.tabs.updates" },
  { value: "storage", labelKey: "settings.tabs.storage" },
  { value: "about", labelKey: "settings.tabs.about" },
];

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * Settings, opened from the top bar gear or Ctrl+comma. Five sections in a vertical tab list. Every control
 * saves when it changes, so there is no Save button.
 */
export function SettingsDialog({ open, onOpenChange }: SettingsDialogProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<SettingsTab>("general");
  const settings = useSettingsStore((state) => state.settings);
  const loadError = useSettingsStore((state) => state.loadError);
  const load = useSettingsStore((state) => state.load);

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      size="lg"
      title={t("settings.title")}
      description={t("settings.description")}
    >
      {settings ? (
        <Tabs
          orientation="vertical"
          value={tab}
          onValueChange={(next) => setTab(next as SettingsTab)}
          className="h-[min(32rem,calc(100vh-13rem))] gap-4"
        >
          <TabsList aria-label={t("settings.sections")} className="w-36 gap-0.5">
            {TABS.map((item) => (
              <TabsTrigger key={item.value} value={item.value} className="font-normal">
                {t(item.labelKey)}
              </TabsTrigger>
            ))}
          </TabsList>
          {/* One frame for every section: a long section scrolls inside it, so the dialog never changes height. */}
          <div className="min-w-0 flex-1 overflow-y-auto pr-1">
            <TabsContent value="general">
              <GeneralTab />
            </TabsContent>
            <TabsContent value="performance">
              <PerformanceTab />
            </TabsContent>
            <TabsContent value="updates">
              <UpdatesTab />
            </TabsContent>
            <TabsContent value="storage">
              <StorageTab />
            </TabsContent>
            <TabsContent value="about">
              <AboutTab />
            </TabsContent>
          </div>
        </Tabs>
      ) : loadError ? (
        <div className="flex flex-col items-start gap-3">
          <p role="alert" className="text-sm text-danger">
            {t("settings.loadFailed", { error: loadError })}
          </p>
          <Button variant="secondary" onClick={() => void load()}>
            {t("actions.tryAgain")}
          </Button>
        </div>
      ) : (
        <p className="text-sm text-fg-muted">{t("settings.loading")}</p>
      )}
    </Dialog>
  );
}

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "@/features/settings/settingsStore";
import type { RecentProject } from "@/ipc/bindings/RecentProject";
import { recentProjects } from "@/ipc/commands";

/** Shown when the settings have not loaded yet. Matches the default in settings_get. */
const DEFAULT_RECENT_LIMIT = 10;

/**
 * Recent `.loopcut` files, listed under the Open video action in the empty monitor. The list is limited by the
 * recent-projects setting and renders nothing when there are no entries or the lookup fails. It shows three rows and
 * the name of a fourth, which says the list scrolls, so a short monitor keeps the empty state in view.
 */
export function RecentProjects({ onOpen }: { onOpen: (path: string) => void }) {
  const { t } = useTranslation();
  const limit = useSettingsStore(
    (state) => state.settings?.general.recentLimit ?? DEFAULT_RECENT_LIMIT,
  );
  const [items, setItems] = useState<RecentProject[]>([]);

  useEffect(() => {
    let cancelled = false;
    recentProjects()
      .then((list) => {
        if (!cancelled) setItems(list);
      })
      .catch(() => {
        if (!cancelled) setItems([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const visible = items.slice(0, Math.max(0, limit));
  if (visible.length === 0) return null;

  return (
    <div className="flex w-full min-w-0 flex-col gap-1 text-left">
      <h3 className="text-xs font-medium text-fg-muted">{t("import.recent")}</h3>
      <ul className="flex flex-col">
        {visible.map((item) => (
          <li key={item.path} className="min-w-0">
            <button
              type="button"
              title={item.path}
              onClick={() => onOpen(item.path)}
              className="flex w-full min-w-0 flex-col rounded-control px-2 py-1 text-left transition-colors duration-150 ease-out-quick hover:bg-hover active:bg-raised"
            >
              <span className="truncate text-sm text-fg">{item.name}</span>
              <span className="truncate text-xs text-fg-muted">{item.path}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

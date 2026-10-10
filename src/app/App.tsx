import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { spikeFlags } from "@/ipc/commands";
import { Toaster, toast } from "@/ui";
import { Shell } from "./Shell";

/** Top-level editor: the shell plus the toast host. Panels live in Shell and its children. */
export function App() {
  const { t } = useTranslation();

  // Diagnostics run only when the app was started for them: the M0 spikes (LOOPCUT_RUN_SPIKES=1) and the real-app
  // self-test (a media file to test). Each module is loaded inside its own branch, so normal startup never pays for
  // the benchmark or the self-test code.
  useEffect(() => {
    let cancelled = false;
    spikeFlags()
      .then(async (flags) => {
        if (cancelled) return;
        if (flags.run) {
          const { runSpikes } = await import("@/features/diagnostics/spikes");
          // The spikes draw into a canvas they own; it does not need to be on screen.
          const canvas = document.createElement("canvas");
          canvas.width = 640;
          canvas.height = 360;
          toast({ title: t("diagnostics.running") });
          const path = await runSpikes(canvas);
          toast({ variant: "success", title: t("diagnostics.written", { path }) });
        }
        const mediaPath = flags.selftest;
        if (typeof mediaPath === "string" && mediaPath.length > 0 && !cancelled) {
          const outDir = flags.selftestOut ?? "";
          if (outDir.length === 0) {
            toast({ variant: "error", title: t("diagnostics.selftestNoFolder") });
            return;
          }
          const { runSelftest } = await import("@/features/diagnostics/selftest");
          toast({ title: t("diagnostics.selftestRunning") });
          const result = await runSelftest({ mediaPath, outDir });
          const passed = result.steps.filter((step) => step.ok).length;
          toast({
            variant: result.ok ? "success" : "warn",
            title: t("diagnostics.selftestWritten", { path: result.path }),
            description: t("diagnostics.selftestSummary", { passed, total: result.steps.length }),
            duration: 12000,
          });
        }
      })
      .catch((error: unknown) => {
        toast({ variant: "error", title: t("diagnostics.failed", { error: String(error) }) });
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  return (
    <>
      <Shell />
      <Toaster />
    </>
  );
}

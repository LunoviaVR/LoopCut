import { invoke, isTauri } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { runSpikes } from "@/features/diagnostics/spikes";

/** Top-level editor shell. Panels are filled in by later milestones. */
export function App() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    invoke<{ run: boolean }>("spike_flags")
      .then(async (flags) => {
        if (!flags.run || cancelled || !canvasRef.current) return;
        setStatus("Running diagnostics…");
        const path = await runSpikes(canvasRef.current);
        setStatus(`Diagnostics written to ${path}`);
      })
      .catch((e) => setStatus(`Diagnostics failed: ${String(e)}`));
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="grid h-full grid-rows-[40px_1fr_220px] bg-bg">
      <header className="flex items-center gap-3 border-b border-line bg-panel px-3">
        <span className="font-semibold tracking-tight">LoopCut</span>
        {status && <span className="text-fg-muted text-xs">{status}</span>}
      </header>
      <main className="grid min-h-0 grid-cols-[260px_1fr_300px]">
        <aside className="border-r border-line bg-panel" aria-label="Media" />
        <section
          className="flex min-h-0 items-center justify-center bg-preview"
          aria-label="Preview"
        >
          <canvas ref={canvasRef} className="max-h-full max-w-full" width={640} height={360} />
        </section>
        <aside className="border-l border-line bg-panel" aria-label="Inspector" />
      </main>
      <section className="border-t border-line bg-panel" aria-label="Timeline" />
    </div>
  );
}

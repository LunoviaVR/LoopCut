import { Download } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "@/domain/bytes";
import { messageOf } from "@/features/import/errors";
import { useSettingsStore } from "@/features/settings/settingsStore";
import type { ExportFormat } from "@/ipc/bindings/ExportFormat";
import type { ExportSettings } from "@/ipc/bindings/ExportSettings";
import type { GifEngine } from "@/ipc/bindings/GifEngine";
import type { PlatformPreset } from "@/ipc/bindings/PlatformPreset";
import type { Project } from "@/ipc/bindings/Project";
import { revealInFolder } from "@/ipc/commands";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/state/editor";
import { useEditGesture } from "@/state/gesture";
import {
  Badge,
  Button,
  Field,
  NumberField,
  ProgressBar,
  Section,
  SegmentedControl,
  Select,
  SliderField,
  Switch,
  toast,
} from "@/ui";
import { requestEstimate, useEstimateStore } from "./estimateSync";
import { cancelExport, startExport } from "./exportFlow";
import {
  bytesToMegabytes,
  DITHERS,
  ditherDevice,
  EXPORT_FORMATS,
  engineUsesPalette,
  etaSeconds,
  extensionOf,
  type FitStatus,
  fitStatus,
  formatElapsed,
  GIF_ENGINES,
  megabytesToBytes,
  sizeLimit,
} from "./exportFormat";
import { useExportStore } from "./exportStore";
import { applyPlatformPreset, usePlatformPresets } from "./platformPresets";

/** The default upload limit for a new target size, in decimal megabytes. */
const DEFAULT_TARGET_MB = 10;

/** A label column of 7rem, the same as the inspector's field rows, over a value column. */
const KEY_VALUE_GRID = "grid grid-cols-[7rem_minmax(0,1fr)] items-baseline gap-x-3 gap-y-1 text-sm";

type QualityField = "quality" | "lossyQuality" | "motionQuality";

export interface ExportPanelProps {
  project: Project;
}

/**
 * Export tool: the format and the GIF engine and its settings, the platform presets, a target size, the live estimate
 * judged against the limit, and the export itself with its progress. Settings writes are undoable like any other edit,
 * and a preset is one undo step.
 */
export function ExportPanel({ project }: ExportPanelProps) {
  const { t } = useTranslation();
  const settings = project.export;
  const estimate = useEstimateStore((state) => state.estimate);
  const estimateError = useEstimateStore((state) => state.error);
  const estimatePending = useEstimateStore((state) => state.pending);
  const { presets, error: presetError } = usePlatformPresets();
  const preset = presets?.find((item) => item.id === settings.preset) ?? null;
  const limit = sizeLimit(settings, preset);
  const fit = fitStatus(estimate, limit);

  const setExport = (patch: Partial<ExportSettings>) => useEditorStore.getState().setExport(patch);

  return (
    <>
      <Section title={t("export.format.title")}>
        <Field label={t("export.format.label")}>
          <Select
            aria-label={t("export.format.label")}
            value={settings.format}
            onValueChange={(value) => setExport({ format: value as ExportFormat })}
            options={EXPORT_FORMATS.map((item) => ({ value: item.value, label: t(item.labelKey) }))}
          />
        </Field>

        {settings.format === "gif" ? <GifSettings settings={settings} /> : null}
      </Section>

      <Section title={t("export.platform.title")}>
        <Field label={t("export.platform.label")}>
          <Select
            aria-label={t("export.platform.label")}
            value={settings.preset ?? "none"}
            onValueChange={(value) => {
              if (value === "none") {
                setExport({ preset: null });
                return;
              }
              const next = presets?.find((item) => item.id === value);
              if (next) applyPlatformPreset(next);
            }}
            options={[
              { value: "none", label: t("export.platform.none") },
              ...(presets ?? []).map((item) => ({ value: item.id, label: item.name })),
            ]}
          />
        </Field>
        {presetError ? (
          <p role="alert" className="text-xs text-danger">
            {t("export.platform.failed", { message: presetError })}
          </p>
        ) : null}
        {preset ? <PresetDetails preset={preset} /> : null}
      </Section>

      <Section title={t("export.target.title")}>
        <TargetSize settings={settings} presetLimit={preset?.maxBytes ?? null} />
      </Section>

      <Section title={t("export.estimate.title")}>
        {estimate ? (
          <dl className={KEY_VALUE_GRID}>
            <dt className="text-fg-muted">{t("export.estimate.size")}</dt>
            <dd className="tabular text-fg">
              {formatBytes(estimate.bytes)}
              <span className="block text-xs text-fg-muted">
                {t("export.estimate.range", {
                  low: formatBytes(estimate.low),
                  high: formatBytes(estimate.high),
                })}
              </span>
            </dd>
            <dt className="text-fg-muted">{t("export.estimate.frames")}</dt>
            <dd className="tabular text-fg">{estimate.frames}</dd>
            <dt className="text-fg-muted">{t("export.estimate.judged")}</dt>
            <dd className="min-w-0">
              <FitBadge status={fit} />
              {/* Without a limit the badge already says so; the caption only names a limit that exists. */}
              {limit !== null ? (
                <span className="block truncate text-xs text-fg-muted">
                  {t("export.estimate.limit", { size: formatBytes(limit) })}
                </span>
              ) : null}
              {fit === "over" && settings.targetBytes !== null ? (
                <span className="block text-xs text-fg-muted">
                  {t("export.estimate.overWillShrink")}
                </span>
              ) : null}
            </dd>
          </dl>
        ) : estimatePending ? (
          <p className="flex items-center gap-2 text-sm text-fg-muted">
            {t("inspector.export.estimating")}
          </p>
        ) : estimateError ? (
          <EstimateFailure project={project} message={estimateError} />
        ) : (
          <p className="text-sm text-fg-muted">{t("export.estimate.waiting")}</p>
        )}
        {estimate?.basis === "sample" ? (
          <p className="text-xs text-fg-muted">{t("export.estimate.sampleNote")}</p>
        ) : null}
      </Section>

      <Section title={t("export.run.title")}>
        <ExportAction project={project} />
      </Section>
    </>
  );
}

/** A slider bound to one quality setting. A drag is one undo step. */
function QualitySlider({
  field,
  label,
  defaultValue,
  settings,
  hint,
}: {
  field: QualityField;
  label: string;
  defaultValue: number;
  settings: ExportSettings;
  hint?: string;
}) {
  const edit = useEditGesture();
  const write = (value: number) => useEditorStore.getState().setExport({ [field]: value });
  return (
    <>
      <SliderField
        label={label}
        value={settings[field]}
        min={1}
        max={100}
        step={1}
        decimals={0}
        defaultValue={defaultValue}
        onChange={(value) => edit.change(() => write(value))}
        onCommit={(value) => edit.commit(() => write(value))}
      />
      {hint ? <p className="text-xs text-fg-muted">{hint}</p> : null}
    </>
  );
}

function GifSettings({ settings }: { settings: ExportSettings }) {
  const { t } = useTranslation();
  const engine = settings.gifEngine;
  const capabilities = useSettingsStore((state) => state.capabilities);
  const hw = useSettingsStore((state) => state.settings?.hw ?? null);
  const write = (patch: Partial<ExportSettings>) => useEditorStore.getState().setExport(patch);
  const colorsEdit = useEditGesture();

  // The GPU hint names the device the dithering runs on, as the hardware settings pick it, so the user is not told
  // about a GPU the export never uses.
  let engineHint: string | null = null;
  if (engine === "gpuDither") {
    const device = ditherDevice(capabilities, hw);
    if (device.kind === "gpu") engineHint = t("export.gif.engineGpuDevice", { name: device.name });
    else if (device.kind === "cpu") engineHint = t("export.gif.engineGpuOff");
    else engineHint = t("export.gif.engineGpuUntested");
  } else {
    const hintKey = GIF_ENGINES.find((item) => item.value === engine)?.hintKey;
    engineHint = hintKey ? t(hintKey) : null;
  }

  return (
    <>
      <div className="flex flex-col gap-1 py-0.5">
        <span className="text-sm text-fg-muted">{t("export.gif.engine")}</span>
        <SegmentedControl<GifEngine>
          className="w-full"
          aria-label={t("export.gif.engine")}
          value={engine}
          onValueChange={(next) => write({ gifEngine: next })}
          items={GIF_ENGINES.map((item) => ({ value: item.value, label: t(item.labelKey) }))}
        />
        {engineHint ? <p className="text-xs text-fg-muted">{engineHint}</p> : null}
      </div>

      <QualitySlider
        field="quality"
        label={t("export.gif.quality")}
        defaultValue={90}
        settings={settings}
      />

      {engineUsesPalette(engine) ? (
        <>
          <NumberField
            label={t("export.gif.colors")}
            value={settings.colors}
            min={2}
            max={256}
            step={1}
            decimals={0}
            onChange={(value) => colorsEdit.change(() => write({ colors: value }))}
            onCommit={(value) => colorsEdit.commit(() => write({ colors: value }))}
          />
          <Field label={t("export.gif.dither")}>
            <Select
              aria-label={t("export.gif.dither")}
              value={settings.dither}
              onValueChange={(next) => write({ dither: next as ExportSettings["dither"] })}
              options={DITHERS.map((item) => ({ value: item.value, label: t(item.labelKey) }))}
            />
          </Field>
        </>
      ) : (
        <>
          <QualitySlider
            field="lossyQuality"
            label={t("export.gif.lossy")}
            defaultValue={100}
            settings={settings}
            hint={t("export.gif.lossyHint")}
          />
          <QualitySlider
            field="motionQuality"
            label={t("export.gif.motion")}
            defaultValue={100}
            settings={settings}
            hint={t("export.gif.motionHint")}
          />
        </>
      )}

      <p className="text-xs text-fg-muted">{t("export.gif.cpuNote")}</p>
    </>
  );
}

function PresetDetails({ preset }: { preset: PlatformPreset }) {
  const { t } = useTranslation();
  const dimensions =
    preset.maxWidth !== null && preset.maxHeight !== null
      ? t("export.preset.dimensionsValue", { width: preset.maxWidth, height: preset.maxHeight })
      : t("export.preset.noLimit");
  return (
    <div className="flex flex-col gap-2 pt-1">
      <dl className={KEY_VALUE_GRID}>
        <dt className="text-fg-muted">{t("export.preset.maxSize")}</dt>
        <dd className="tabular text-fg">
          {preset.maxBytes === null ? t("export.preset.noLimit") : formatBytes(preset.maxBytes)}
        </dd>
        <dt className="text-fg-muted">{t("export.preset.dimensions")}</dt>
        <dd className="tabular text-fg">{dimensions}</dd>
        <dt className="text-fg-muted">{t("export.preset.frames")}</dt>
        <dd className="tabular text-fg">
          {preset.maxFrames === null ? t("export.preset.noLimit") : preset.maxFrames}
        </dd>
        <dt className="text-fg-muted">{t("export.preset.frameRate")}</dt>
        <dd className="tabular text-fg">
          {preset.maxFps === null
            ? t("export.preset.noLimit")
            : t("export.preset.fpsValue", { value: preset.maxFps })}
        </dd>
        <dt className="text-fg-muted">{t("export.preset.recommended")}</dt>
        <dd className="tabular text-fg">
          {t("export.preset.recommendedValue", {
            width: preset.recommendedWidth,
            fps: preset.recommendedFps,
          })}
        </dd>
        <dt className="text-fg-muted">{t("export.preset.checked")}</dt>
        <dd className="flex items-center gap-2 text-fg">
          <span className="tabular">{preset.checkedOn}</span>
          <Badge variant={preset.verified ? "ok" : "neutral"}>
            {preset.verified ? t("export.preset.verified") : t("export.preset.unverified")}
          </Badge>
        </dd>
      </dl>
      <p className="text-xs text-fg-muted">{preset.note}</p>
    </div>
  );
}

function TargetSize({
  settings,
  presetLimit,
}: {
  settings: ExportSettings;
  presetLimit: number | null;
}) {
  const { t } = useTranslation();
  const edit = useEditGesture();
  const on = settings.targetBytes !== null;
  const fallbackMb = presetLimit !== null ? bytesToMegabytes(presetLimit) : DEFAULT_TARGET_MB;
  const write = (bytes: number | null) =>
    useEditorStore.getState().setExport({ targetBytes: bytes });

  return (
    <>
      <Switch
        checked={on}
        onCheckedChange={(next) => write(next ? megabytesToBytes(fallbackMb) : null)}
        label={t("export.target.switch")}
        description={t("export.target.hint")}
      />
      {on && settings.targetBytes !== null ? (
        <NumberField
          label={t("export.target.megabytes")}
          value={bytesToMegabytes(settings.targetBytes)}
          min={0.1}
          max={1000}
          step={0.1}
          decimals={1}
          unit="MB"
          onChange={(value) => edit.change(() => write(megabytesToBytes(value)))}
          onCommit={(value) => edit.commit(() => write(megabytesToBytes(value)))}
        />
      ) : null}
    </>
  );
}

/** The fit verdict. Its words are the same as the inspector's status row (Fits, Near the limit, Over the limit). */
export function FitBadge({ status }: { status: FitStatus }) {
  const { t } = useTranslation();
  if (status === "none") return <Badge variant="neutral">{t("export.estimate.noLimit")}</Badge>;
  if (status === "fits") return <Badge variant="ok">{t("inspector.export.fits")}</Badge>;
  if (status === "near") return <Badge variant="warn">{t("inspector.export.near")}</Badge>;
  return <Badge variant="danger">{t("inspector.export.over")}</Badge>;
}

/** The estimate failed: the reason as visible text, and Try again, which asks for the estimate once more. */
function EstimateFailure({ project, message }: { project: Project; message: string }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-1">
        <p role="alert" className="text-sm text-danger">
          {t("inspector.export.estimateUnavailable")}
        </p>
        <p className="break-words text-xs text-fg-muted">{message}</p>
      </div>
      <div>
        <Button variant="secondary" size="sm" onClick={() => requestEstimate(project)}>
          {t("export.estimate.tryAgain")}
        </Button>
      </div>
    </div>
  );
}

/** The export button, and the progress or outcome of the job it started. */
function ExportAction({ project }: { project: Project }) {
  const { t } = useTranslation();
  const exporting = useEditorStore((state) => state.isExporting);
  const status = useExportStore((state) => state.status);
  const running = status === "running" || status === "cancelling";
  const extension = extensionOf(project.export.format).toUpperCase();

  return (
    <div className="flex flex-col gap-2">
      <Button
        variant="primary"
        size="md"
        disabled={exporting}
        onClick={() => void startExport()}
        leadingIcon={<Download size={16} strokeWidth={1.5} aria-hidden="true" />}
      >
        {t("export.run.start", { format: extension })}
      </Button>
      {running ? <ExportProgress /> : null}
      {status === "done" ? <ExportOutcome /> : null}
      {status === "failed" ? <ExportFailure /> : null}
      {status === "cancelled" ? (
        <p className="text-sm text-fg-muted">{t("export.cancelled")}</p>
      ) : null}
    </div>
  );
}

/** The failed job: the line that says so, then the message and the technical detail the backend stored with it. */
function ExportFailure() {
  const { t } = useTranslation();
  const message = useExportStore((state) => state.error);
  const detail = useExportStore((state) => state.errorDetail);
  return (
    <div role="alert" className="flex flex-col gap-1">
      <p className="text-sm text-danger">{t("export.run.failed")}</p>
      {message ? <p className="break-words text-xs text-fg-muted">{message}</p> : null}
      {detail ? (
        <p className="tabular break-words text-xs text-fg-muted whitespace-pre-wrap">{detail}</p>
      ) : null}
    </div>
  );
}

/** The current time in milliseconds, refreshed twice a second while `active`. */
function useClock(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

function ExportProgress() {
  const { t } = useTranslation();
  const status = useExportStore((state) => state.status);
  const framesDone = useExportStore((state) => state.framesDone);
  const framesTotal = useExportStore((state) => state.framesTotal);
  const bytes = useExportStore((state) => state.bytes);
  const startedAt = useExportStore((state) => state.startedAt);
  const passes = useExportStore((state) => state.passes);
  const fallbacks = useExportStore((state) => state.fallbacks);
  const now = useClock(true);
  const elapsed = startedAt === null ? 0 : (now - startedAt) / 1000;
  const eta = etaSeconds(elapsed, framesDone, framesTotal);
  const cancelling = status === "cancelling";

  return (
    <div className="flex flex-col gap-1.5" aria-live="polite">
      <ProgressBar
        label={t("export.progress.label")}
        value={framesTotal > 0 ? framesDone / framesTotal : 0}
        indeterminate={framesTotal === 0}
      />
      <p className="tabular text-xs text-fg-muted">
        {framesTotal > 0
          ? t("export.progress.frames", { done: framesDone, total: framesTotal })
          : t("export.progress.starting")}
        {bytes > 0 ? ` · ${formatBytes(bytes)}` : ""}
      </p>
      <p className="tabular text-xs text-fg-muted">
        {t("export.progress.elapsed", { time: formatElapsed(elapsed) })}
        {eta !== null && !cancelling
          ? ` · ${t("export.progress.eta", { time: formatElapsed(eta) })}`
          : ""}
      </p>
      {fallbacks > 0 ? (
        <p className="text-xs text-warn">{t("export.progress.fallbacks", { count: fallbacks })}</p>
      ) : null}
      {passes.map((pass) => (
        <p key={pass.pass} className="tabular text-xs text-fg-muted">
          {t("export.progress.pass", {
            pass: pass.pass,
            settings: pass.settings,
            size: formatBytes(pass.bytes),
          })}
        </p>
      ))}
      <div>
        <Button
          variant="secondary"
          size="sm"
          disabled={cancelling}
          onClick={() => void cancelExport()}
        >
          {cancelling ? t("export.progress.cancelling") : t("export.progress.cancel")}
        </Button>
      </div>
    </div>
  );
}

function ExportOutcome() {
  const { t } = useTranslation();
  const result = useExportStore((state) => state.result);
  if (!result) return null;
  const name = result.path.split(/[\\/]/).pop() ?? result.path;
  return (
    <div className="flex flex-col gap-1.5">
      <p className={cn("text-sm text-ok")}>
        {t("export.outcome.saved", {
          name,
          size: formatBytes(result.bytes),
          seconds: result.seconds.toFixed(1),
        })}
      </p>
      <div>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => {
            revealInFolder(result.path).catch((failure: unknown) => {
              toast({
                variant: "error",
                title: t("export.revealFailed"),
                description: messageOf(failure),
              });
            });
          }}
        >
          {t("export.showInFolder")}
        </Button>
      </div>
    </div>
  );
}

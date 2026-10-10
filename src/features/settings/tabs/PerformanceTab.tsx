import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { codecName } from "@/features/media/mediaInfo";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { CapStatus } from "@/ipc/bindings/CapStatus";
import type { DecodeCap } from "@/ipc/bindings/DecodeCap";
import type { DecodePreference } from "@/ipc/bindings/DecodePreference";
import type { DeviceCaps } from "@/ipc/bindings/DeviceCaps";
import type { EncodeCap } from "@/ipc/bindings/EncodeCap";
import type { EncoderPreference } from "@/ipc/bindings/EncoderPreference";
import type { GpuVendor } from "@/ipc/bindings/GpuVendor";
import type { HwSettings } from "@/ipc/bindings/HwSettings";
import type { VideoCodec } from "@/ipc/bindings/VideoCodec";
import { hwProbe } from "@/ipc/commands";
import {
  Badge,
  Button,
  ProgressBar,
  SectionHeading,
  SegmentedControl,
  Select,
  Switch,
  toast,
} from "@/ui";
import {
  type Availability,
  decodeAvailability,
  decodeRowsFor,
  encodeRowsFor,
  encoderAvailability,
  type GpuOption,
  gpuDevices,
  gpuOptions,
  gpuValueFor,
  gpuValueOf,
  tableCodecs,
} from "../hw";
import { SettingRow } from "../SettingRow";
import { useSettingsStore } from "../settingsStore";

const DECODERS: readonly { value: DecodePreference; labelKey: string }[] = [
  { value: "auto", labelKey: "settings.perf.decodeAuto" },
  { value: "d3d11va", labelKey: "settings.perf.decodeD3d11va" },
  { value: "cuda", labelKey: "settings.perf.decodeNvdec" },
  { value: "qsv", labelKey: "settings.perf.decodeQsv" },
  { value: "amf", labelKey: "settings.perf.decodeAmf" },
  { value: "cpu", labelKey: "settings.perf.decodeOff" },
];

const ENCODERS: readonly { value: EncoderPreference; labelKey: string }[] = [
  { value: "auto", labelKey: "settings.perf.encoderAuto" },
  { value: "cpu", labelKey: "settings.perf.encoderCpu" },
  { value: "nvenc", labelKey: "settings.perf.encoderNvenc" },
  { value: "qsv", labelKey: "settings.perf.encoderQsv" },
  { value: "amf", labelKey: "settings.perf.encoderAmf" },
];

const VENDOR_KEY: Record<GpuVendor | "any", string> = {
  nvidia: "settings.perf.vendorNvidia",
  amd: "settings.perf.vendorAmd",
  intel: "settings.perf.vendorIntel",
  microsoft: "settings.perf.vendorSoftware",
  other: "settings.perf.vendorOther",
  any: "settings.perf.vendorAny",
};

/** Translation keys for the FFmpeg backend names the probe reports. Other names are shown as they are. */
const BACKEND_KEY: Record<string, string> = {
  d3d11va: "settings.perf.decodeD3d11va",
  cuda: "settings.perf.decodeNvdec",
  qsv: "settings.perf.decodeQsv",
  amf: "settings.perf.decodeAmf",
  nvenc: "settings.perf.encoderNvenc",
};

interface ProbeProgress {
  label: string;
  value: number;
}

/**
 * Hardware acceleration. Each choice is checked against the last hardware test: a decoder or encoder the test
 * did not pass is listed but disabled, with the reason beside it. The note at the bottom states what the GPU
 * does and does not speed up.
 */
export function PerformanceTab() {
  const { t } = useTranslation();
  const settings = useSettingsStore((state) => state.settings);
  const capabilities = useSettingsStore((state) => state.capabilities);
  const refreshCapabilities = useSettingsStore((state) => state.refreshCapabilities);
  const update = useSettingsStore((state) => state.update);
  const [testing, setTesting] = useState(false);
  const [progress, setProgress] = useState<ProbeProgress | null>(null);

  useEffect(() => {
    void refreshCapabilities();
  }, [refreshCapabilities]);

  if (!settings) return null;
  const hw = settings.hw;
  const enabled = hw.enabled;

  const setHw = <K extends keyof HwSettings>(key: K, value: HwSettings[K]) => {
    void update((draft) => {
      draft.hw[key] = value;
    });
  };

  const reasonOf = (availability: Availability): string | null => {
    if (availability.available) return null;
    const vendor = t(VENDOR_KEY[availability.vendor ?? "any"]);
    if (availability.reason === "untested") return t("settings.perf.reasonUntested");
    if (availability.reason === "no_adapter") {
      return t("settings.perf.reasonNoAdapter", { vendor });
    }
    return t("settings.perf.reasonNotPassed", { vendor });
  };

  const gpus = gpuOptions(capabilities, hw.gpu);
  const selectedGpu = gpus.find((option) => option.value === gpuValueFor(hw.gpu));
  const gpuLabel = (option: GpuOption): string => {
    if (option.kind === "auto") return t("settings.perf.gpuAuto");
    if (option.kind === "software") return t("settings.perf.gpuSoftware");
    return option.stale ? t("settings.perf.gpuNotInTest", { name: option.name }) : option.name;
  };
  const gpuHint = selectedGpu?.stale ? t("settings.perf.gpuStale") : null;

  const decoders = DECODERS.map((item) => {
    const availability = decodeAvailability(capabilities, item.value);
    return { ...item, availability, reason: reasonOf(availability) };
  });
  const encoders = ENCODERS.map((item) => {
    const availability = encoderAvailability(capabilities, item.value);
    return { ...item, availability, reason: reasonOf(availability) };
  });
  const unavailableDecoders = decoders.filter((item) => item.reason !== null);

  const encoderOptions = (
    items: typeof encoders,
  ): { value: EncoderPreference; label: string; disabled: boolean }[] =>
    items.map((item) => ({
      value: item.value,
      label: t(item.labelKey),
      disabled: !item.availability.available,
    }));

  const runTest = async () => {
    setTesting(true);
    setProgress(null);
    try {
      const result = await hwProbe(true, (event) => {
        if (event.kind === "step") {
          setProgress({
            label: event.label,
            value: event.total > 0 ? event.done / event.total : 0,
          });
        } else if (!event.ok) {
          toast({
            variant: "warn",
            title: t("settings.perf.stepFailed", { step: event.label }),
          });
        }
      });
      useSettingsStore.getState().setCapabilities(result);
      toast({ variant: "success", title: t("settings.perf.testDone") });
    } catch (error) {
      toast({
        variant: "error",
        title: t("settings.perf.testFailed"),
        description: messageOf(error),
        duration: 12000,
      });
    } finally {
      setTesting(false);
      setProgress(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <Switch
        checked={enabled}
        onCheckedChange={(on) => setHw("enabled", on)}
        label={t("settings.perf.enabled")}
        description={t("settings.perf.enabledBody")}
      />

      <section aria-label={t("settings.perf.devices")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.perf.devices")} />
        <SettingRow label={t("settings.perf.gpu")} hint={gpuHint}>
          <Select
            aria-label={t("settings.perf.gpu")}
            disabled={!enabled}
            value={gpuValueFor(hw.gpu)}
            options={gpus.map((option) => ({
              value: option.value,
              label: gpuLabel(option),
            }))}
            onValueChange={(value) => {
              const option = gpus.find((item) => item.value === value);
              if (option) setHw("gpu", option.preference);
            }}
          />
        </SettingRow>
      </section>

      <section aria-label={t("settings.perf.decoding")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.perf.decoding")} />
        <SettingRow
          label={t("settings.perf.decoder")}
          hint={
            !capabilities ? (
              t("settings.perf.decodersUntested")
            ) : unavailableDecoders.length > 0 ? (
              <ul className="flex flex-col gap-0.5">
                {unavailableDecoders.map((item) => (
                  <li key={item.value}>
                    {t(item.labelKey)}: {item.reason}
                  </li>
                ))}
              </ul>
            ) : null
          }
        >
          <Select
            aria-label={t("settings.perf.decoder")}
            disabled={!enabled}
            value={hw.decode}
            options={decoders.map((item) => ({
              value: item.value,
              label: t(item.labelKey),
              disabled: !item.availability.available,
            }))}
            onValueChange={(value) => setHw("decode", value)}
          />
        </SettingRow>
      </section>

      <section aria-label={t("settings.perf.encoding")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.perf.encoding")} />
        <SettingRow
          label={t("settings.perf.proxyEncoder")}
          hint={t("settings.perf.proxyEncoderBody")}
        >
          <Select
            aria-label={t("settings.perf.proxyEncoder")}
            disabled={!enabled}
            value={hw.proxyEncoder}
            options={encoderOptions(encoders)}
            onValueChange={(value) => setHw("proxyEncoder", value)}
          />
        </SettingRow>
        <SettingRow
          label={t("settings.perf.exportEncoder")}
          hint={t("settings.perf.exportEncoderBody")}
        >
          <Select
            aria-label={t("settings.perf.exportEncoder")}
            disabled={!enabled}
            value={hw.exportEncoder}
            options={encoderOptions(encoders)}
            onValueChange={(value) => setHw("exportEncoder", value)}
          />
        </SettingRow>
      </section>

      <section aria-label={t("settings.perf.effects")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.perf.effects")} />
        <SettingRow
          label={t("settings.perf.effectsRenderer")}
          hint={t("settings.perf.effectsBody")}
        >
          <SegmentedControl
            aria-label={t("settings.perf.effectsRenderer")}
            disabled={!enabled}
            value={hw.effects}
            onValueChange={(value) => setHw("effects", value)}
            items={[
              { value: "gpu", label: t("settings.perf.effectsGpu") },
              { value: "cpu", label: t("settings.perf.effectsCpu") },
            ]}
          />
        </SettingRow>
        <Switch
          checked={hw.gpuScaling}
          disabled={!enabled}
          onCheckedChange={(on) => setHw("gpuScaling", on)}
          label={t("settings.perf.gpuScaling")}
          description={t("settings.perf.gpuScalingBody")}
        />
      </section>

      <p className="rounded-control bg-raised px-3 py-2 text-xs text-fg-muted">
        {t("settings.perf.cpuNote")}
      </p>

      <section aria-label={t("settings.perf.capabilities")} className="flex flex-col gap-2">
        <SectionHeading
          title={t("settings.perf.capabilities")}
          actions={
            <Button variant="secondary" size="sm" loading={testing} onClick={() => void runTest()}>
              {t("settings.perf.runTest")}
            </Button>
          }
        />
        {testing && progress ? (
          <div className="flex flex-col gap-1">
            <span className="text-xs text-fg-muted">
              {t("settings.perf.testingStep", { step: progress.label })}
            </span>
            <ProgressBar label={t("settings.perf.testProgress")} value={progress.value} />
          </div>
        ) : null}
        {capabilities ? (
          <>
            <p className="text-xs text-fg-muted">
              {t("settings.perf.testedAt", {
                date: new Date(capabilities.probedAtUnix * 1000).toLocaleString(),
                version: capabilities.ffmpegVersion,
              })}
            </p>
            {capabilities.warnings.map((warning) => (
              <p key={warning} className="text-xs text-warn">
                {warning}
              </p>
            ))}
            <CapabilityTable capabilities={capabilities} />
          </>
        ) : (
          <p className="text-sm text-fg-muted">{t("settings.perf.noResults")}</p>
        )}
      </section>
    </div>
  );
}

/** The FFmpeg backend's display name, or the raw name when the app has no label for it. */
function useBackendLabel(): (backend: string) => string {
  const { t } = useTranslation();
  return (backend) => {
    const key = BACKEND_KEY[backend];
    return key ? t(key) : backend;
  };
}

/** One probe result: the name, its outcome, and the error when it failed. */
function CapabilityChip({
  label,
  status,
  error,
  details,
}: {
  label: string;
  status: CapStatus;
  error: string | null;
  details?: string | null;
}) {
  const { t } = useTranslation();
  const variant = status === "ok" ? "ok" : status === "failed" ? "danger" : "neutral";
  const statusLabel =
    status === "ok"
      ? t("settings.perf.statusOk")
      : status === "failed"
        ? t("settings.perf.statusFailed")
        : t("settings.perf.statusUnsupported");

  return (
    <span className="flex min-w-0 flex-wrap items-center gap-2">
      <Badge variant={variant} title={error ?? undefined}>
        {t("settings.perf.chip", { label, status: statusLabel })}
      </Badge>
      {details ? <span className="tabular text-xs text-fg-muted">{details}</span> : null}
    </span>
  );
}

function DecodeCell({ rows }: { rows: DecodeCap[] }) {
  const { t } = useTranslation();
  const backendLabel = useBackendLabel();
  if (rows.length === 0) {
    return <span className="text-xs text-fg-muted">{t("settings.perf.notTested")}</span>;
  }
  return (
    <ul className="flex flex-col gap-1">
      {rows.map((row) => (
        <li key={`${row.backend}-${row.tenBit ? "10" : "8"}`}>
          <CapabilityChip
            label={
              row.tenBit
                ? t("settings.perf.tenBitLabel", { backend: backendLabel(row.backend) })
                : backendLabel(row.backend)
            }
            status={row.status}
            error={row.error}
          />
        </li>
      ))}
    </ul>
  );
}

function EncodeCell({ rows }: { rows: EncodeCap[] }) {
  const { t } = useTranslation();
  const backendLabel = useBackendLabel();
  if (rows.length === 0) {
    return <span className="text-xs text-fg-muted">{t("settings.perf.notTested")}</span>;
  }
  return (
    <ul className="flex flex-col gap-1">
      {rows.map((row) => {
        const details =
          row.minWidth && row.minHeight
            ? t("settings.perf.minSize", { width: row.minWidth, height: row.minHeight })
            : null;
        return (
          <li key={row.backend}>
            <CapabilityChip
              label={backendLabel(row.backend)}
              status={row.status}
              error={row.error}
              details={details}
            />
          </li>
        );
      })}
    </ul>
  );
}

/** One GPU: its codec rows (decode and encode per backend) and the GPU scalers the test tried. */
function DeviceTable({ entry, codecs }: { entry: DeviceCaps; codecs: VideoCodec[] }) {
  const { t } = useTranslation();
  const backendLabel = useBackendLabel();
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex min-w-0 items-center gap-2">
        <h4 className="min-w-0 truncate text-sm font-medium text-fg">{entry.device.key.name}</h4>
        <Badge variant="neutral">{t(VENDOR_KEY[entry.device.vendor])}</Badge>
      </div>
      <table className="w-full border-collapse text-left text-sm">
        <thead>
          <tr className="text-xs text-fg-muted">
            <th scope="col" className="w-16 py-1 font-normal">
              {t("settings.perf.codec")}
            </th>
            <th scope="col" className="py-1 font-normal">
              {t("settings.perf.decode")}
            </th>
            <th scope="col" className="py-1 font-normal">
              {t("settings.perf.encode")}
            </th>
          </tr>
        </thead>
        <tbody>
          {codecs.map((codec) => (
            <tr key={codec} className="border-t border-line">
              <th scope="row" className="py-1.5 pr-2 text-left font-normal text-fg">
                {codecName(codec)}
              </th>
              <td className="py-1.5 pr-2 align-middle">
                <DecodeCell rows={decodeRowsFor(entry, codec)} />
              </td>
              <td className="py-1.5 align-middle">
                <EncodeCell rows={encodeRowsFor(entry, codec)} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {entry.scale.length > 0 ? (
        <ul className="flex flex-col gap-1 pt-1">
          {entry.scale.map((scaler) => (
            <li key={`${scaler.scaler}-${scaler.backend}`}>
              <CapabilityChip
                label={t("settings.perf.scaleOn", {
                  codec: codecName(scaler.codec),
                  backend: backendLabel(scaler.backend),
                })}
                status={scaler.status}
                error={scaler.error}
              />
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/** The CPU encoders the test ran: libx264, libx265, libsvtav1 and libvpx-vp9. */
function CpuEncoders({ capabilities }: { capabilities: Capabilities }) {
  const { t } = useTranslation();
  if (capabilities.cpuEncode.length === 0) return null;
  return (
    <div className="flex flex-col gap-1.5">
      <h4 className="text-sm font-medium text-fg">{t("settings.perf.cpuEncoders")}</h4>
      <ul className="flex flex-col gap-1">
        {capabilities.cpuEncode.map((row) => (
          <li key={row.backend} className="flex min-w-0 items-center gap-3">
            <span className="w-16 shrink-0 text-sm text-fg">{codecName(row.codec)}</span>
            <CapabilityChip label={row.backend} status={row.status} error={row.error} />
          </li>
        ))}
      </ul>
    </div>
  );
}

/** One table per GPU, then the CPU encoders. Software rasterizers have no probes and are not listed. */
export function CapabilityTable({ capabilities }: { capabilities: Capabilities }) {
  const codecs = tableCodecs(capabilities);
  return (
    <div className="flex flex-col gap-4">
      {gpuDevices(capabilities).map((entry) => (
        <DeviceTable key={gpuValueOf(entry.device.key)} entry={entry} codecs={codecs} />
      ))}
      <CpuEncoders capabilities={capabilities} />
    </div>
  );
}

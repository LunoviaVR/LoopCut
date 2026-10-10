/**
 * Pure rules behind the Performance tab: which GPUs, decoders and encoders the last hardware test allows, and
 * the rows of the capability table. Components turn the reason codes into copy.
 *
 * The probe reports one `DeviceCaps` per adapter. Each decode and encode row names its FFmpeg backend
 * (`d3d11va`, `amf`, `nvenc`, ...), and the rules below match preferences against those names.
 */
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { DecodeCap } from "@/ipc/bindings/DecodeCap";
import type { DecodePreference } from "@/ipc/bindings/DecodePreference";
import type { DeviceCaps } from "@/ipc/bindings/DeviceCaps";
import type { EncodeCap } from "@/ipc/bindings/EncodeCap";
import type { EncoderPreference } from "@/ipc/bindings/EncoderPreference";
import type { GpuKey } from "@/ipc/bindings/GpuKey";
import type { GpuPreference } from "@/ipc/bindings/GpuPreference";
import type { GpuVendor } from "@/ipc/bindings/GpuVendor";
import type { VideoCodec } from "@/ipc/bindings/VideoCodec";

/** Codecs that always get a row in the capability table. VP9 gets one only when a probe reported it. */
export const TABLE_CODECS: readonly VideoCodec[] = ["h264", "hevc", "av1", "vp9"];

/** Why a backend is unavailable. `vendor` names the GPU maker the backend needs. */
export type UnavailableReason = "untested" | "no_adapter" | "not_passed";

export interface Availability {
  available: boolean;
  reason: UnavailableReason | null;
  vendor: GpuVendor | "any" | null;
}

const AVAILABLE: Availability = { available: true, reason: null, vendor: null };

/** The hardware adapters (the Microsoft software rasterizer is not a choice). */
export function gpuDevices(capabilities: Capabilities | null): DeviceCaps[] {
  return (capabilities?.devices ?? []).filter((entry) => !entry.device.isSoftware);
}

export function gpuValueOf(key: GpuKey): string {
  return `gpu:${key.vendorId}:${key.deviceId}:${key.ordinal}`;
}

export interface GpuOption {
  value: string;
  preference: GpuPreference;
  kind: "auto" | "software" | "gpu";
  /** Adapter name for GPU options; empty for Auto and CPU only. */
  name: string;
  /** True for the current choice when the last test did not report it. */
  stale: boolean;
}

/**
 * GPU selector options: Auto, each GPU the last test reported, CPU only. The current choice stays in the list
 * even when the probe no longer reports it, so the control never shows an empty value.
 */
export function gpuOptions(
  capabilities: Capabilities | null,
  selected: GpuPreference,
): GpuOption[] {
  const options: GpuOption[] = [
    { value: "auto", preference: { mode: "auto" }, kind: "auto", name: "", stale: false },
  ];
  const seen = new Set<string>();
  for (const { device } of gpuDevices(capabilities)) {
    const value = gpuValueOf(device.key);
    seen.add(value);
    options.push({
      value,
      preference: { mode: "specific", key: device.key },
      kind: "gpu",
      name: device.key.name,
      stale: false,
    });
  }
  options.push({
    value: "software",
    preference: { mode: "software" },
    kind: "software",
    name: "",
    stale: false,
  });

  if (selected.mode === "specific" && !seen.has(gpuValueOf(selected.key))) {
    options.splice(1, 0, {
      value: gpuValueOf(selected.key),
      preference: selected,
      kind: "gpu",
      name: selected.key.name,
      stale: true,
    });
  }
  return options;
}

/** The option that matches the stored GPU preference. */
export function gpuValueFor(preference: GpuPreference): string {
  switch (preference.mode) {
    case "auto":
      return "auto";
    case "software":
      return "software";
    case "specific":
      return gpuValueOf(preference.key);
  }
}

/** FFmpeg decode backend and the GPU vendor each preference needs. `any` accepts every vendor. */
const DECODE_BACKEND: Partial<
  Record<DecodePreference, { backend: string; vendor: GpuVendor | "any" }>
> = {
  d3d11va: { backend: "d3d11va", vendor: "any" },
  cuda: { backend: "cuda", vendor: "nvidia" },
  qsv: { backend: "qsv", vendor: "intel" },
  amf: { backend: "amf", vendor: "amd" },
};

/** Whether a decoder can be chosen, from the last hardware test. */
export function decodeAvailability(
  capabilities: Capabilities | null,
  preference: DecodePreference,
): Availability {
  const target = DECODE_BACKEND[preference];
  if (!target) return AVAILABLE;
  if (!capabilities) return { available: false, reason: "untested", vendor: target.vendor };
  const adapters = gpuDevices(capabilities).filter(
    (entry) => target.vendor === "any" || entry.device.vendor === target.vendor,
  );
  if (adapters.length === 0)
    return { available: false, reason: "no_adapter", vendor: target.vendor };
  const passed = adapters.some((entry) =>
    entry.decode.some((row) => row.backend === target.backend && row.status === "ok"),
  );
  if (!passed) return { available: false, reason: "not_passed", vendor: target.vendor };
  return AVAILABLE;
}

/** The encoder backend name FFmpeg uses for each hardware encoder preference. */
const ENCODE_BACKEND: Partial<Record<EncoderPreference, { backend: string; vendor: GpuVendor }>> = {
  nvenc: { backend: "nvenc", vendor: "nvidia" },
  amf: { backend: "amf", vendor: "amd" },
  qsv: { backend: "qsv", vendor: "intel" },
};

/** Whether a hardware encoder can be chosen, from the encode rows of the last hardware test. CPU is always available. */
export function encoderAvailability(
  capabilities: Capabilities | null,
  preference: EncoderPreference,
): Availability {
  const target = ENCODE_BACKEND[preference];
  if (!target) return AVAILABLE;
  if (!capabilities) return { available: false, reason: "untested", vendor: target.vendor };
  const adapters = gpuDevices(capabilities).filter(
    (entry) => entry.device.vendor === target.vendor,
  );
  if (adapters.length === 0)
    return { available: false, reason: "no_adapter", vendor: target.vendor };
  const rows = adapters
    .flatMap((entry) => entry.encode)
    .filter((row) => row.backend === target.backend);
  if (rows.length === 0) return { available: false, reason: "no_adapter", vendor: target.vendor };
  if (!rows.some((row) => row.status === "ok")) {
    return { available: false, reason: "not_passed", vendor: target.vendor };
  }
  return AVAILABLE;
}

/**
 * Codecs to show in the capability table, in table order. H.264, HEVC and AV1 always appear; VP9 appears only
 * when a probe reported it (on a GPU or as a CPU encoder).
 */
export function tableCodecs(capabilities: Capabilities): VideoCodec[] {
  const mentioned = new Set<VideoCodec>();
  for (const entry of capabilities.devices) {
    for (const row of [...entry.decode, ...entry.encode]) mentioned.add(row.codec);
  }
  for (const row of capabilities.cpuEncode) mentioned.add(row.codec);
  return TABLE_CODECS.filter((codec) => codec !== "vp9" || mentioned.has(codec));
}

/** The decode rows of one codec on one device, in probe order (one per backend and bit depth). */
export function decodeRowsFor(entry: DeviceCaps, codec: VideoCodec): DecodeCap[] {
  return entry.decode.filter((row) => row.codec === codec);
}

/** The encode rows of one codec on one device, in probe order (one per hardware encoder). */
export function encodeRowsFor(entry: DeviceCaps, codec: VideoCodec): EncodeCap[] {
  return entry.encode.filter((row) => row.codec === codec);
}

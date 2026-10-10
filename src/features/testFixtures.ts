/**
 * Synthetic data for the feature tests. Nothing here describes a real file or account: the paths, names and
 * numbers are invented for tests only. Not a test file itself.
 */

import type { AvailableUpdate } from "@/features/updates/updateCheck";
import type { AppSettings } from "@/ipc/bindings/AppSettings";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";

export function sampleMedia(overrides: Partial<ImportedMedia> = {}): ImportedMedia {
  return {
    token: "tok-test-1",
    path: "C:\\Clips\\ranked-ace.mp4",
    fileName: "ranked-ace.mp4",
    info: {
      container: "mp4",
      duration: 12.4,
      sizeBytes: 21_800_000,
      video: {
        index: 0,
        codec: "h264",
        profile: "High",
        width: 1920,
        height: 1080,
        sampleAspect: { num: 1, den: 1 },
        pixFmt: "yuv420p",
        bitDepth: 8,
        hasAlpha: false,
        rotation: 0,
        frameRate: { num: 30, den: 1 },
        avgFrameRate: { num: 30, den: 1 },
        variableFrameRate: false,
        color: { matrix: "bt709", range: "limited", transfer: "sdr", wideGamut: false },
        startTime: 0,
        duration: 12.4,
        frameCount: 372,
        interlaced: false,
        webPlayable: true,
      },
      alternateVideoStreams: [],
      hasAudio: true,
    },
    sourceUrl: null,
    needsProxy: false,
    proxyUrl: null,
    keyframeInterval: 60,
    ...overrides,
  };
}

export function sampleSettings(): AppSettings {
  return {
    hw: {
      enabled: true,
      gpu: { mode: "auto" },
      decode: "auto",
      proxyEncoder: "auto",
      exportEncoder: "auto",
      effects: "gpu",
      gpuScaling: true,
    },
    updates: { checkOnStartup: true, skippedVersion: null, lastCheck: null },
    cache: { maxDiskGb: 10, ramBudgetMb: 1024 },
    general: { defaultExportDir: null, recentLimit: 10 },
  };
}

/** One AMD GPU that passed H.264 decode and encode, plus the software rasterizer; no NVIDIA or Intel adapter. */
export function sampleCapabilities(): Capabilities {
  return {
    schema: 1,
    ffmpegVersion: "n9.0.2-25-g67b60c310b-20261010",
    probedAtUnix: 1_791_652_800,
    devices: [
      {
        device: {
          key: { vendorId: 0x1002, deviceId: 0x7590, ordinal: 0, name: "AMD Radeon RX 9060 XT" },
          vendor: "amd",
          dxgiIndex: 0,
          luid: 1,
          dedicatedVramMb: 16_112,
          driverVersion: "32.0.31041.1004",
          isSoftware: false,
          cudaIndex: null,
        },
        decode: [
          { codec: "h264", tenBit: false, backend: "amf", status: "ok", speedX: null, error: null },
          {
            codec: "h264",
            tenBit: false,
            backend: "d3d11va",
            status: "ok",
            speedX: null,
            error: null,
          },
        ],
        scale: [],
        encode: [
          {
            codec: "h264",
            backend: "amf",
            status: "ok",
            minWidth: 256,
            minHeight: 144,
            speedX: null,
            error: null,
          },
        ],
      },
      {
        device: {
          key: {
            vendorId: 0x1414,
            deviceId: 0x008c,
            ordinal: 0,
            name: "Microsoft Basic Render Driver",
          },
          vendor: "microsoft",
          dxgiIndex: 1,
          luid: 2,
          dedicatedVramMb: 0,
          driverVersion: null,
          isSoftware: true,
          cudaIndex: null,
        },
        decode: [],
        scale: [],
        encode: [],
      },
    ],
    cpuEncode: [
      {
        codec: "h264",
        backend: "libx264",
        status: "ok",
        minWidth: null,
        minHeight: null,
        speedX: null,
        error: null,
      },
    ],
    warnings: [],
  };
}

/** A stand-in for the updater's `Update` resource, with a scripted download. */
export function fakeUpdate(
  overrides: Partial<AvailableUpdate> & { body?: string | null } = {},
): AvailableUpdate {
  return {
    version: "0.0.2",
    currentVersion: "0.0.1",
    body: "Faster previews.",
    date: null,
    downloadAndInstall: async (onEvent) => {
      onEvent({ event: "Started", data: { contentLength: 100 } });
      onEvent({ event: "Progress", data: { chunkLength: 40 } });
      onEvent({ event: "Progress", data: { chunkLength: 60 } });
      onEvent({ event: "Finished" });
    },
    close: async () => {},
    ...overrides,
  };
}

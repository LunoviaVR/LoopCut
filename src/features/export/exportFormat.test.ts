import { describe, expect, it } from "vitest";
import { defaultExportSettings } from "@/domain/defaults";
import { sampleCapabilities, sampleSettings } from "@/features/testFixtures";
import type { PlatformPreset } from "@/ipc/bindings/PlatformPreset";
import type { SizeEstimate } from "@/ipc/types";
import {
  bytesToMegabytes,
  ditherDevice,
  etaSeconds,
  extensionOf,
  fitStatus,
  folderOf,
  formatElapsed,
  joinPath,
  megabytesToBytes,
  presetPlan,
  sizeLimit,
  suggestedExportName,
} from "./exportFormat";

const estimate: SizeEstimate = {
  bytes: 3_565_000,
  low: 3_280_000,
  high: 3_850_000,
  frames: 120,
  basis: "sample",
};

const discord: PlatformPreset = {
  id: "discord",
  name: "Discord",
  format: "gif",
  maxBytes: 10_000_000,
  maxWidth: null,
  maxHeight: null,
  maxFrames: null,
  maxFps: 50,
  recommendedWidth: 480,
  recommendedFps: 20,
  verified: false,
  checkedOn: "2026-10-10",
  source: null,
  note: "note",
};

describe("file names and folders", () => {
  it("names the file after the project with the format's extension", () => {
    expect(extensionOf("gif")).toBe("gif");
    expect(extensionOf("mp4H264")).toBe("mp4");
    expect(extensionOf("mp4Hevc")).toBe("mp4");
    expect(extensionOf("webmVp9")).toBe("webm");
    expect(extensionOf("webmAv1")).toBe("webm");
    expect(suggestedExportName("Ranked ace", "gif")).toBe("Ranked ace.gif");
  });

  it("replaces characters Windows refuses and trailing dots in the name", () => {
    // A run of unsafe characters becomes one underscore.
    expect(suggestedExportName('Clip: "one"?', "gif")).toBe("Clip_ _one_.gif");
    expect(suggestedExportName("   ", "mp4H264")).toBe("export.mp4");
    expect(suggestedExportName("name..", "gif")).toBe("name.gif");
  });

  it("joins the folder and the name with the folder's own separator", () => {
    expect(joinPath("C:\\Clips", "a.gif")).toBe("C:\\Clips\\a.gif");
    expect(joinPath("C:\\Clips\\", "a.gif")).toBe("C:\\Clips\\a.gif");
    expect(joinPath("/home/me/gifs/", "a.gif")).toBe("/home/me/gifs/a.gif");
    expect(joinPath(null, "a.gif")).toBe("a.gif");
    expect(joinPath("", "a.gif")).toBe("a.gif");
  });
});

describe("size status", () => {
  it("fits when the high end is under the limit", () => {
    expect(fitStatus(estimate, 10_000_000)).toBe("fits");
  });

  it("is near when only the likely size is under the limit", () => {
    expect(fitStatus(estimate, 3_700_000)).toBe("near");
  });

  it("is over when the likely size is above the limit", () => {
    expect(fitStatus(estimate, 3_000_000)).toBe("over");
  });

  it("has no status without an estimate or a limit", () => {
    expect(fitStatus(null, 10_000_000)).toBe("none");
    expect(fitStatus(estimate, null)).toBe("none");
  });

  it("judges against the target size when one is set, else the preset's limit", () => {
    const settings = defaultExportSettings();
    expect(sizeLimit(settings, discord)).toBe(10_000_000);
    expect(sizeLimit({ ...settings, targetBytes: 4_000_000 }, discord)).toBe(4_000_000);
    expect(sizeLimit(settings, null)).toBeNull();
  });
});

describe("preset plan", () => {
  it("sets the format, the recommended width and frame rate, the limit as the target, and the id", () => {
    expect(presetPlan(discord)).toEqual({
      presetId: "discord",
      format: "gif",
      width: 480,
      fps: 20,
      targetBytes: 10_000_000,
    });
  });

  it("leaves the target off when the preset has no upload limit", () => {
    expect(presetPlan({ ...discord, maxBytes: null }).targetBytes).toBeNull();
  });
});

describe("progress arithmetic", () => {
  it("converts megabytes to decimal bytes and back", () => {
    expect(megabytesToBytes(10)).toBe(10_000_000);
    expect(bytesToMegabytes(2_500_000)).toBe(2.5);
  });

  it("formats elapsed time as m:ss, or h:mm:ss past an hour", () => {
    expect(formatElapsed(0)).toBe("0:00");
    expect(formatElapsed(65.4)).toBe("1:05");
    expect(formatElapsed(3723)).toBe("1:02:03");
    expect(formatElapsed(-4)).toBe("0:00");
  });

  it("estimates the time left from the rate so far", () => {
    expect(etaSeconds(10, 25, 100)).toBeCloseTo(30, 9);
    expect(etaSeconds(10, 100, 100)).toBe(0);
    expect(etaSeconds(10, 0, 100)).toBeNull();
    expect(etaSeconds(10, 5, 0)).toBeNull();
  });
});

describe("the folder of a chosen file", () => {
  it("names the folder with either separator style and nothing for a bare name", () => {
    expect(folderOf("D:\\Clips\\Out\\loop.gif")).toBe("D:\\Clips\\Out");
    expect(folderOf("/home/clips/loop.gif")).toBe("/home/clips");
    expect(folderOf("loop.gif")).toBeNull();
  });
});

describe("ditherDevice", () => {
  const hw = sampleSettings().hw;
  const amd = sampleCapabilities().devices[0]?.device.key;
  if (!amd) throw new Error("the sample has a GPU");

  it("names the first hardware adapter in DXGI order under Auto", () => {
    expect(ditherDevice(sampleCapabilities(), hw)).toEqual({
      kind: "gpu",
      name: "AMD Radeon RX 9060 XT",
    });
  });

  it("names the pinned GPU when the last test listed it", () => {
    const pinned = { ...hw, gpu: { mode: "specific" as const, key: amd } };
    expect(ditherDevice(sampleCapabilities(), pinned)).toEqual({
      kind: "gpu",
      name: "AMD Radeon RX 9060 XT",
    });
  });

  it("is unknown for a pinned GPU the last test did not list", () => {
    const gone = { ...hw, gpu: { mode: "specific" as const, key: { ...amd, deviceId: 1 } } };
    expect(ditherDevice(sampleCapabilities(), gone)).toEqual({ kind: "unknown" });
  });

  it("is unknown until a hardware test has run, and until the settings load", () => {
    expect(ditherDevice(null, hw)).toEqual({ kind: "unknown" });
    expect(ditherDevice(sampleCapabilities(), null)).toEqual({ kind: "unknown" });
  });

  it("runs on the CPU when hardware acceleration is off, whatever the test found", () => {
    expect(ditherDevice(sampleCapabilities(), { ...hw, enabled: false })).toEqual({ kind: "cpu" });
  });

  it("runs on the CPU when the effects renderer or the GPU choice is software", () => {
    expect(ditherDevice(sampleCapabilities(), { ...hw, effects: "cpu" })).toEqual({ kind: "cpu" });
    expect(ditherDevice(sampleCapabilities(), { ...hw, gpu: { mode: "software" } })).toEqual({
      kind: "cpu",
    });
  });

  it("runs on the CPU when the last test found no hardware adapter", () => {
    const capabilities = sampleCapabilities();
    capabilities.devices = capabilities.devices.filter((entry) => entry.device.isSoftware);
    expect(ditherDevice(capabilities, hw)).toEqual({ kind: "cpu" });
  });

  it("does not need a passing probe: the renderer picks the adapter from the list alone", () => {
    const capabilities = sampleCapabilities();
    for (const entry of capabilities.devices) {
      entry.decode = entry.decode.map((row) => ({ ...row, status: "failed" }));
      entry.encode = entry.encode.map((row) => ({ ...row, status: "unsupported" }));
    }
    expect(ditherDevice(capabilities, hw)).toEqual({
      kind: "gpu",
      name: "AMD Radeon RX 9060 XT",
    });
  });
});

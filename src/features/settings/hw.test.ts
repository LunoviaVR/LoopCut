import "@/ui/test/env";
import { describe, expect, it } from "vitest";
import { sampleCapabilities } from "@/features/testFixtures";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { DeviceCaps } from "@/ipc/bindings/DeviceCaps";
import type { GpuKey } from "@/ipc/bindings/GpuKey";
import {
  decodeAvailability,
  encoderAvailability,
  gpuOptions,
  gpuValueFor,
  gpuValueOf,
  tableCodecs,
} from "./hw";

const AMD_KEY: GpuKey = {
  vendorId: 0x1002,
  deviceId: 0x7590,
  ordinal: 0,
  name: "AMD Radeon RX 9060 XT",
};

const NVIDIA: DeviceCaps = {
  device: {
    key: { vendorId: 0x10de, deviceId: 0x2786, ordinal: 0, name: "NVIDIA GeForce RTX 4070" },
    vendor: "nvidia",
    dxgiIndex: 1,
    luid: 3,
    dedicatedVramMb: 12_000,
    driverVersion: null,
    isSoftware: false,
    cudaIndex: 0,
  },
  decode: [
    { codec: "h264", tenBit: false, backend: "cuda", status: "ok", speedX: null, error: null },
  ],
  scale: [],
  encode: [
    {
      codec: "h264",
      backend: "nvenc",
      status: "failed",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: "NVENC refused the session",
    },
  ],
};

function withNvidia(capabilities: Capabilities, nvidia: DeviceCaps = NVIDIA): Capabilities {
  return { ...capabilities, devices: [...capabilities.devices, nvidia] };
}

describe("decoder availability", () => {
  it("always allows Auto and Off", () => {
    expect(decodeAvailability(null, "auto").available).toBe(true);
    expect(decodeAvailability(null, "cpu").available).toBe(true);
  });

  it("asks for a hardware test before offering a GPU decoder", () => {
    expect(decodeAvailability(null, "amf")).toEqual({
      available: false,
      reason: "untested",
      vendor: "amd",
    });
  });

  it("names the vendor that is missing from the last test", () => {
    expect(decodeAvailability(sampleCapabilities(), "cuda")).toEqual({
      available: false,
      reason: "no_adapter",
      vendor: "nvidia",
    });
  });

  it("offers an AMD decoder when an AMD GPU decoded through AMF in the test", () => {
    expect(decodeAvailability(sampleCapabilities(), "amf").available).toBe(true);
  });

  it("accepts D3D11VA on any GPU that decoded through it", () => {
    expect(decodeAvailability(sampleCapabilities(), "d3d11va").available).toBe(true);
  });

  it("reports a GPU that is present but failed every decode row for the backend", () => {
    const failing: DeviceCaps = {
      ...NVIDIA,
      decode: [
        {
          codec: "h264",
          tenBit: false,
          backend: "cuda",
          status: "failed",
          speedX: null,
          error: "cuvid init",
        },
      ],
    };
    expect(decodeAvailability(withNvidia(sampleCapabilities(), failing), "cuda")).toEqual({
      available: false,
      reason: "not_passed",
      vendor: "nvidia",
    });
  });
});

describe("encoder availability", () => {
  it("matches encoders by the backend names the probe reports", () => {
    expect(encoderAvailability(sampleCapabilities(), "amf").available).toBe(true);
    expect(encoderAvailability(sampleCapabilities(), "nvenc")).toEqual({
      available: false,
      reason: "no_adapter",
      vendor: "nvidia",
    });
  });

  it("reports an encoder that the test listed but did not pass", () => {
    expect(encoderAvailability(withNvidia(sampleCapabilities()), "nvenc")).toEqual({
      available: false,
      reason: "not_passed",
      vendor: "nvidia",
    });
  });

  it("always allows the CPU encoders", () => {
    expect(encoderAvailability(null, "cpu").available).toBe(true);
  });
});

describe("GPU choices", () => {
  it("lists Auto, the GPUs the test reported and CPU only; the software renderer is not a GPU", () => {
    const options = gpuOptions(sampleCapabilities(), { mode: "auto" });
    expect(options.map((option) => option.kind)).toEqual(["auto", "gpu", "software"]);
    expect(options[1]?.preference).toEqual({ mode: "specific", key: AMD_KEY });
  });

  it("keeps a stored GPU in the list when the last test did not report it", () => {
    const selected = { mode: "specific" as const, key: NVIDIA.device.key };
    const options = gpuOptions(sampleCapabilities(), selected);
    const stale = options.find((option) => option.stale);
    expect(stale?.name).toBe("NVIDIA GeForce RTX 4070");
    expect(gpuValueFor(selected)).toBe(gpuValueOf(NVIDIA.device.key));
  });

  it("names a GPU by vendor, device and ordinal so identical cards stay apart", () => {
    expect(gpuValueFor({ mode: "specific", key: AMD_KEY })).toBe("gpu:4098:30096:0");
    expect(gpuValueOf({ ...AMD_KEY, ordinal: 1 })).not.toBe(gpuValueOf(AMD_KEY));
  });
});

describe("capability table", () => {
  it("lists H.264, HEVC and AV1 always, and VP9 only when a probe reported it", () => {
    expect(tableCodecs(sampleCapabilities())).toEqual(["h264", "hevc", "av1"]);
    const withVp9 = sampleCapabilities();
    withVp9.cpuEncode.push({
      codec: "vp9",
      backend: "libvpx-vp9",
      status: "ok",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: null,
    });
    expect(tableCodecs(withVp9)).toEqual(["h264", "hevc", "av1", "vp9"]);
  });
});

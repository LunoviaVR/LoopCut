import "@/ui/test/env";
import { describe, expect, it } from "vitest";
import { codecName, displaySize, hdrFormat, matrixName, rangeKey } from "./mediaInfo";

describe("displaySize", () => {
  it("keeps the stored size when there is no quarter turn", () => {
    expect(displaySize({ width: 1920, height: 1080, rotation: 0 })).toEqual({
      width: 1920,
      height: 1080,
      rotated: false,
    });
    expect(displaySize({ width: 1920, height: 1080, rotation: 180 })).toEqual({
      width: 1920,
      height: 1080,
      rotated: false,
    });
  });

  it("swaps the sides for a quarter turn so phone video reads as portrait", () => {
    expect(displaySize({ width: 1920, height: 1080, rotation: 90 })).toEqual({
      width: 1080,
      height: 1920,
      rotated: true,
    });
    expect(displaySize({ width: 1920, height: 1080, rotation: 270 }).rotated).toBe(true);
  });
});

describe("labels", () => {
  it("names codecs and matrices the way the spec writes them", () => {
    expect(codecName("h264")).toBe("H.264");
    expect(codecName("hevc")).toBe("HEVC");
    expect(codecName("prores")).toBe("ProRes");
    expect(codecName("mjpeg")).toBe("MJPEG");
    expect(matrixName("bt709")).toBe("BT.709");
    expect(matrixName("bt2020")).toBe("BT.2020");
  });

  it("maps color ranges to translation keys", () => {
    expect(rangeKey("limited")).toBe("rangeLimited");
    expect(rangeKey("full")).toBe("rangeFull");
  });

  it("reports HDR formats and nothing for SDR", () => {
    expect(hdrFormat("pq")).toBe("PQ");
    expect(hdrFormat("hlg")).toBe("HLG");
    expect(hdrFormat("sdr")).toBeNull();
  });
});

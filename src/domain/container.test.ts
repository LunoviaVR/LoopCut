import { describe, expect, it } from "vitest";
import { containerLabel } from "@/domain/container";

/** The format name FFmpeg reports for every MP4 and QuickTime file. */
const MOV_FAMILY = "mov,mp4,m4a,3gp,3g2,mj2";

describe("containerLabel", () => {
  it("names the MP4 and QuickTime family by the file's extension", () => {
    expect(containerLabel(MOV_FAMILY, "clip.mp4")).toBe("MP4");
    expect(containerLabel(MOV_FAMILY, "clip.m4v")).toBe("MP4");
    expect(containerLabel(MOV_FAMILY, "clip.mov")).toBe("MOV");
    expect(containerLabel(MOV_FAMILY, "clip.3gp")).toBe("3GP");
    expect(containerLabel(MOV_FAMILY, "voice.m4a")).toBe("M4A");
  });

  it("falls back to MP4 in the family when the extension names none of its members", () => {
    expect(containerLabel(MOV_FAMILY, "clip")).toBe("MP4");
    expect(containerLabel(MOV_FAMILY, "clip.unknown")).toBe("MP4");
  });

  it("matches the extension without regard to case", () => {
    expect(containerLabel(MOV_FAMILY, "CLIP.MOV")).toBe("MOV");
  });

  it("names Matroska and WebM by the file's extension", () => {
    expect(containerLabel("matroska,webm", "clip.webm")).toBe("WebM");
    expect(containerLabel("matroska,webm", "clip.mkv")).toBe("MKV");
  });

  it("names AVI, GIF, MPEG-TS, FLV and Ogg", () => {
    expect(containerLabel("avi", "clip.avi")).toBe("AVI");
    expect(containerLabel("gif", "loop.gif")).toBe("GIF");
    expect(containerLabel("mpegts", "broadcast.ts")).toBe("MPEG-TS");
    expect(containerLabel("flv", "old.flv")).toBe("FLV");
    expect(containerLabel("ogg", "theora.ogv")).toBe("Ogg");
  });

  it("names ASF files WMV for .wmv and ASF otherwise", () => {
    expect(containerLabel("asf", "clip.wmv")).toBe("WMV");
    expect(containerLabel("asf", "clip.asf")).toBe("ASF");
  });

  it("shows the first format name in capitals for a format the table does not name", () => {
    expect(containerLabel("unknown", "clip.xyz")).toBe("UNKNOWN");
    expect(containerLabel("swf,extra", "anim.swf")).toBe("SWF");
  });
});

import { describe, expect, it } from "vitest";
import { defaultCaptionStyle } from "@/domain/defaults";
import { sampleMedia } from "@/features/testFixtures";
import type { Caption } from "@/ipc/bindings/Caption";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import { sampleProject } from "@/state/test-fixtures";
import {
  captionBox,
  captionFont,
  captionLines,
  clampCaptionCentre,
  fontPixels,
  outputSizeFor,
  rgbaCss,
} from "./captionGeometry";

/** Half an em per character, in pixels: a predictable measure for the box tests. */
const measure = (text: string, _font: string, fontPx: number) => text.length * fontPx * 0.5;

function caption(overrides: Partial<Caption> = {}): Caption {
  return {
    id: "cap-1",
    text: "Hello",
    range: { start: 0, end: 2 },
    x: 0.5,
    y: 0.5,
    style: { ...defaultCaptionStyle(), strokeWidth: 0, background: null, maxWidth: 1 },
    animation: { kind: "none" },
    ...overrides,
  };
}

describe("caption lines and fonts", () => {
  it("splits text on line breaks and uppercases when the style says so", () => {
    const upper = caption({ text: "one\ntwo" });
    expect(captionLines(upper)).toEqual(["ONE", "TWO"]);
    const plain = caption({
      text: "one\r\ntwo",
      style: { ...defaultCaptionStyle(), uppercase: false },
    });
    expect(captionLines(plain)).toEqual(["one", "two"]);
  });

  it("builds the CSS font from the style and the output height", () => {
    const style = {
      ...defaultCaptionStyle(),
      fontSize: 0.1,
      bold: true,
      italic: true,
      fontFamily: "Segoe UI",
    };
    expect(captionFont(style, 300)).toBe('italic bold 30.00px "Segoe UI"');
    expect(fontPixels({ ...style, fontSize: 0 }, 300)).toBe(1);
  });

  it("formats colors as CSS rgba with straight alpha", () => {
    expect(rgbaCss([255, 0, 0, 255])).toBe("rgba(255, 0, 0, 1.000)");
    expect(rgbaCss([0, 0, 0, 0])).toBe("rgba(0, 0, 0, 0.000)");
  });
});

describe("caption box", () => {
  it("centres the box on the anchor, as fractions of the frame", () => {
    const box = captionBox(caption({ x: 0.5, y: 0.25 }), { width: 480, height: 270 }, measure);
    expect(box.left + box.width / 2).toBeCloseTo(0.5, 9);
    expect(box.top + box.height / 2).toBeCloseTo(0.25, 9);
  });

  it("sizes the box from the widest line and the line count", () => {
    const style = {
      ...defaultCaptionStyle(),
      fontSize: 0.1,
      strokeWidth: 0,
      background: null,
      maxWidth: 1,
      lineHeight: 1,
    };
    const output = { width: 400, height: 200 };
    const box = captionBox(caption({ text: "abcd\nab", style }), output, measure);
    // 20 px font: "ABCD" is 4 * 20 * 0.5 = 40 px wide, two lines of 20 px each.
    expect(box.width * output.width).toBeCloseTo(40, 6);
    expect(box.height * output.height).toBeCloseTo(40, 6);
  });

  it("adds the outline and the background padding on both sides", () => {
    const plain = captionBox(caption(), { width: 400, height: 200 }, measure);
    const boxed = captionBox(
      caption({
        style: {
          ...defaultCaptionStyle(),
          strokeWidth: 0.1,
          background: [0, 0, 0, 160],
          maxWidth: 1,
        },
      }),
      { width: 400, height: 200 },
      measure,
    );
    expect(boxed.width).toBeGreaterThan(plain.width);
    expect(boxed.height).toBeGreaterThan(plain.height);
  });

  it("caps the width at the style's maximum width", () => {
    const style = { ...defaultCaptionStyle(), strokeWidth: 0, background: null, maxWidth: 0.2 };
    const box = captionBox(
      caption({ text: "a very long caption line", style }),
      { width: 400, height: 200 },
      measure,
    );
    expect(box.width).toBeCloseTo(0.2, 9);
  });

  it("clamps a centre to the frame", () => {
    expect(clampCaptionCentre(-1, 2)).toEqual({ x: 0, y: 1 });
    expect(clampCaptionCentre(0.3, 0.6)).toEqual({ x: 0.3, y: 0.6 });
  });
});

describe("output size of the frame", () => {
  it("uses the preview's size when a preview exists", () => {
    const preview = { outputWidth: 320, outputHeight: 180 } as PreviewInfo;
    expect(outputSizeFor(sampleProject(), sampleMedia(), preview)).toEqual({
      width: 320,
      height: 180,
    });
  });

  it("works out the width-mode output from the source and the crop", () => {
    expect(outputSizeFor(sampleProject(), sampleMedia(), null)).toEqual({
      width: 480,
      height: 270,
    });
  });

  it("falls back to a default frame without media", () => {
    expect(outputSizeFor(sampleProject(), null, null)).toEqual({ width: 480, height: 270 });
  });
});

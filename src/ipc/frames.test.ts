import "@/ui/test/env";
import { describe, expect, it, vi } from "vitest";
import { drawFrame, encodeFrame, FRAME_HEADER_BYTES, FrameRenderer, parseFrame } from "./frames";

function solid(width: number, height: number, value: number): Uint8ClampedArray {
  return new Uint8ClampedArray(width * height * 4).fill(value);
}

describe("frame wire format", () => {
  it("round-trips width, height and pixels through the 8-byte LE header", () => {
    const rgba = solid(3, 2, 200);
    const buffer = encodeFrame(3, 2, rgba);

    expect(buffer.byteLength).toBe(FRAME_HEADER_BYTES + 3 * 2 * 4);
    const header = new DataView(buffer);
    expect(header.getUint32(0, true)).toBe(3);
    expect(header.getUint32(4, true)).toBe(2);

    const frame = parseFrame(buffer);
    expect(frame.width).toBe(3);
    expect(frame.height).toBe(2);
    expect(frame.rgba.length).toBe(24);
    expect(frame.rgba[0]).toBe(200);
  });

  it("rejects a buffer shorter than the header", () => {
    expect(() => parseFrame(new ArrayBuffer(4))).toThrow(RangeError);
  });

  it("rejects a payload whose length does not match the header dimensions", () => {
    const buffer = encodeFrame(2, 2, solid(2, 2, 0));
    const truncated = buffer.slice(0, buffer.byteLength - 4);

    expect(() => parseFrame(truncated)).toThrow(/needs 24 bytes, got 20/);
  });

  it("refuses to encode pixels that do not fill the stated size", () => {
    expect(() => encodeFrame(4, 4, solid(2, 2, 0))).toThrow(RangeError);
  });
});

describe("drawing", () => {
  it("resizes the canvas only when the frame size changes, then puts the pixels", () => {
    const putImageData = vi.fn();
    const canvas = { width: 0, height: 0 } as HTMLCanvasElement;
    const ctx = { canvas, putImageData } as unknown as CanvasRenderingContext2D;

    drawFrame(ctx, parseFrame(encodeFrame(4, 2, solid(4, 2, 9))));
    expect(canvas.width).toBe(4);
    expect(canvas.height).toBe(2);
    expect(putImageData).toHaveBeenCalledTimes(1);

    drawFrame(ctx, parseFrame(encodeFrame(4, 2, solid(4, 2, 9))));
    expect(putImageData).toHaveBeenCalledTimes(2);
  });

  it("keeps one 2D context for a whole stream", () => {
    const putImageData = vi.fn();
    const ctx = {
      canvas: { width: 0, height: 0 },
      putImageData,
    } as unknown as CanvasRenderingContext2D;
    const getContext = vi.fn(() => ctx);
    const canvas = { getContext } as unknown as HTMLCanvasElement;

    const renderer = new FrameRenderer(canvas);
    const first = renderer.draw(encodeFrame(2, 2, solid(2, 2, 1)));
    renderer.draw(encodeFrame(2, 2, solid(2, 2, 2)));

    expect(first).toEqual({ width: 2, height: 2 });
    expect(getContext).toHaveBeenCalledTimes(1);
    expect(putImageData).toHaveBeenCalledTimes(2);
  });
});

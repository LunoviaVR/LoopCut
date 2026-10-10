/**
 * Preview frame wire format (docs/ipc.md): an 8-byte header (`u32 LE width`, `u32 LE height`) followed by
 * tightly packed straight-alpha RGBA. Decoding is a zero-copy view over the received buffer; drawing
 * reuses one 2D context so a 20 fps stream does not allocate a canvas per frame.
 */

export const FRAME_HEADER_BYTES = 8;

export interface DecodedFrame {
  width: number;
  height: number;
  /** View into the received buffer, starting after the header. */
  rgba: Uint8ClampedArray<ArrayBuffer>;
}

/** Parses one frame. Throws RangeError when the byte length does not match the header. */
export function parseFrame(buffer: ArrayBuffer): DecodedFrame {
  if (buffer.byteLength < FRAME_HEADER_BYTES) {
    throw new RangeError(`frame is ${buffer.byteLength} bytes, shorter than its header`);
  }
  const header = new DataView(buffer);
  const width = header.getUint32(0, true);
  const height = header.getUint32(4, true);
  const pixelBytes = width * height * 4;
  if (buffer.byteLength !== FRAME_HEADER_BYTES + pixelBytes) {
    throw new RangeError(
      `frame ${width}x${height} needs ${FRAME_HEADER_BYTES + pixelBytes} bytes, got ${buffer.byteLength}`,
    );
  }
  return { width, height, rgba: new Uint8ClampedArray(buffer, FRAME_HEADER_BYTES, pixelBytes) };
}

/** Packs RGBA pixels into the wire format. Used by the browser mock and by tests. */
export function encodeFrame(width: number, height: number, rgba: Uint8ClampedArray): ArrayBuffer {
  if (rgba.length !== width * height * 4) {
    throw new RangeError(`expected ${width * height * 4} RGBA bytes, got ${rgba.length}`);
  }
  const buffer = new ArrayBuffer(FRAME_HEADER_BYTES + rgba.length);
  const header = new DataView(buffer);
  header.setUint32(0, width, true);
  header.setUint32(4, height, true);
  new Uint8ClampedArray(buffer, FRAME_HEADER_BYTES).set(rgba);
  return buffer;
}

/** Puts a decoded frame on a 2D context, resizing the backing canvas only when the size changes. */
export function drawFrame(ctx: CanvasRenderingContext2D, frame: DecodedFrame): void {
  const { canvas } = ctx;
  if (canvas.width !== frame.width) canvas.width = frame.width;
  if (canvas.height !== frame.height) canvas.height = frame.height;
  ctx.putImageData(new ImageData(frame.rgba, frame.width, frame.height), 0, 0);
}

/** Draws wire-format frames onto one canvas, keeping its context for the whole stream. */
export class FrameRenderer {
  private readonly ctx: CanvasRenderingContext2D;

  constructor(canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("2D canvas context is unavailable");
    this.ctx = ctx;
  }

  /** Decodes and draws one frame; returns its size so callers can keep the canvas aspect ratio. */
  draw(buffer: ArrayBuffer): { width: number; height: number } {
    const frame = parseFrame(buffer);
    drawFrame(this.ctx, frame);
    return { width: frame.width, height: frame.height };
  }
}

/**
 * Filmstrip thumbnails for the timeline. The Rust side owns the frames (`media_thumbnails` sets up the count and
 * height; `fetchThumbnail` returns one index as RGBA). This module loads only the indices the view asks for,
 * a few at a time, turns each into a cached image once, and reports when a new one is ready so the canvas can
 * redraw. Nothing here touches React.
 */
import type { ThumbnailPixels } from "@/ipc/types";

/** Thumbnails requested per clip, scaled by duration and capped (`media_thumbnails` allows 1 to 512). */
export function thumbnailCountFor(duration: number): number {
  if (!(duration > 0)) return 8;
  return Math.min(240, Math.max(8, Math.ceil(duration * 2)));
}

/** Requested thumbnail height in pixels: the 64px track at the device pixel ratio, even, within 8 to 720. */
export function thumbnailHeightFor(dpr: number): number {
  const raw = Math.round(64 * (Number.isFinite(dpr) && dpr > 0 ? dpr : 1));
  const even = raw - (raw % 2);
  return Math.min(720, Math.max(8, even));
}

export type ThumbnailLoader = (index: number) => Promise<ThumbnailPixels>;
export type ThumbnailDecoder = (pixels: ThumbnailPixels) => Promise<CanvasImageSource>;

/** Turns RGBA into an image the canvas can draw: an ImageBitmap when the browser has one, else a canvas. */
export const decodeThumbnail: ThumbnailDecoder = async (pixels) => {
  const imageData = new ImageData(pixels.rgba, pixels.width, pixels.height);
  if (typeof createImageBitmap === "function") return createImageBitmap(imageData);
  const canvas = document.createElement("canvas");
  canvas.width = pixels.width;
  canvas.height = pixels.height;
  canvas.getContext("2d")?.putImageData(imageData, 0, 0);
  return canvas;
};

type EntryState = "queued" | "loading" | "ready" | "failed";
interface Entry {
  state: EntryState;
  image?: CanvasImageSource;
}

export interface ThumbnailStripOptions {
  /** Number of thumbnails in the set; indices outside `[0, count)` are never loaded. */
  count: number;
  /** Fetches the RGBA of one index. */
  load: ThumbnailLoader;
  /** Converts RGBA to a drawable image. Defaults to `decodeThumbnail`. */
  decode?: ThumbnailDecoder;
  /** Called when a thumbnail becomes drawable, so the owner can redraw. */
  onReady: () => void;
  /** Loads in flight at once. Defaults to 4. */
  concurrency?: number;
}

/**
 * Lazily loaded, cached thumbnails for one media set. `get()` returns the image once it is ready and queues the
 * load on the first call. Failed loads are not retried, so a missing thumbnail draws as a plain placeholder.
 */
export class ThumbnailStrip {
  private readonly entries = new Map<number, Entry>();
  private readonly queue: number[] = [];
  private active = 0;
  private disposed = false;
  private readonly decode: ThumbnailDecoder;
  private readonly concurrency: number;

  constructor(private readonly options: ThumbnailStripOptions) {
    this.decode = options.decode ?? decodeThumbnail;
    this.concurrency = Math.max(1, options.concurrency ?? 4);
  }

  /** The decoded thumbnail for `index`, or null while it loads or after it failed. */
  get(index: number): CanvasImageSource | null {
    if (this.disposed || !Number.isInteger(index)) return null;
    const entry = this.entries.get(index);
    if (entry) return entry.state === "ready" ? (entry.image ?? null) : null;
    if (index < 0 || index >= this.options.count) return null;
    this.entries.set(index, { state: "queued" });
    this.queue.push(index);
    this.pump();
    return null;
  }

  /** Stops loading and closes every decoded image. Results that arrive later are dropped. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.queue.length = 0;
    for (const entry of this.entries.values()) closeImage(entry.image);
    this.entries.clear();
  }

  private pump(): void {
    while (!this.disposed && this.active < this.concurrency) {
      const index = this.queue.shift();
      if (index === undefined) return;
      this.start(index);
    }
  }

  private start(index: number): void {
    this.active += 1;
    this.entries.set(index, { state: "loading" });
    this.options
      .load(index)
      .then((pixels) => this.decode(pixels))
      .then(
        (image) => {
          if (this.disposed) {
            closeImage(image);
            return;
          }
          this.entries.set(index, { state: "ready", image });
          this.options.onReady();
        },
        () => {
          if (!this.disposed) this.entries.set(index, { state: "failed" });
        },
      )
      .finally(() => {
        this.active -= 1;
        this.pump();
      });
  }
}

function closeImage(image: CanvasImageSource | undefined): void {
  if (image && typeof (image as ImageBitmap).close === "function") (image as ImageBitmap).close();
}

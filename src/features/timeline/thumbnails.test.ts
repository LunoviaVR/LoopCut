import { describe, expect, it, vi } from "vitest";
import type { ThumbnailPixels } from "@/ipc/types";
import { ThumbnailStrip, thumbnailCountFor, thumbnailHeightFor } from "./thumbnails";

const pixels = (): ThumbnailPixels => ({ width: 2, height: 2, rgba: new Uint8ClampedArray(16) });

/** A loader whose calls resolve only when the test says so. */
function deferredLoader() {
  const pending: Array<{
    index: number;
    resolve: (value: ThumbnailPixels) => void;
    reject: () => void;
  }> = [];
  const load = vi.fn(
    (index: number) =>
      new Promise<ThumbnailPixels>((resolve, reject) => {
        pending.push({ index, resolve, reject: () => reject(new Error("failed")) });
      }),
  );
  return { load, pending };
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

describe("thumbnail counts and heights", () => {
  it("asks for about two thumbnails a second, between 8 and 240", () => {
    expect(thumbnailCountFor(12.4)).toBe(25);
    expect(thumbnailCountFor(0)).toBe(8);
    expect(thumbnailCountFor(10_000)).toBe(240);
  });

  it("scales the 64px track by the device pixel ratio, always even", () => {
    expect(thumbnailHeightFor(1)).toBe(64);
    expect(thumbnailHeightFor(1.5)).toBe(96);
    expect(thumbnailHeightFor(1.25)).toBe(80);
    expect(thumbnailHeightFor(0)).toBe(64);
  });
});

describe("ThumbnailStrip", () => {
  it("queues a load on first request and reports the image once it is decoded", async () => {
    const { load, pending } = deferredLoader();
    const onReady = vi.fn();
    const decode = vi.fn(async () => ({ tag: "image" }) as unknown as CanvasImageSource);
    const strip = new ThumbnailStrip({ count: 4, load, decode, onReady });

    expect(strip.get(2)).toBeNull();
    expect(load).toHaveBeenCalledTimes(1);
    pending[0]?.resolve(pixels());
    await flush();

    expect(strip.get(2)).toEqual({ tag: "image" });
    expect(onReady).toHaveBeenCalledTimes(1);
  });

  it("never runs more loads at once than the concurrency allows", async () => {
    const { load, pending } = deferredLoader();
    const strip = new ThumbnailStrip({
      count: 10,
      load,
      decode: async () => ({}) as CanvasImageSource,
      onReady: () => undefined,
      concurrency: 2,
    });
    for (let index = 0; index < 5; index++) strip.get(index);
    expect(load).toHaveBeenCalledTimes(2);

    pending[0]?.resolve(pixels());
    await flush();
    expect(load).toHaveBeenCalledTimes(3);
  });

  it("does not retry a failed load and never loads indices outside the set", () => {
    const { load, pending } = deferredLoader();
    const strip = new ThumbnailStrip({
      count: 2,
      load,
      decode: async () => ({}) as CanvasImageSource,
      onReady: () => undefined,
    });
    expect(strip.get(5)).toBeNull();
    expect(load).not.toHaveBeenCalled();

    strip.get(0);
    pending[0]?.reject();
    return flush().then(() => {
      strip.get(0);
      expect(load).toHaveBeenCalledTimes(1);
    });
  });

  it("closes decoded bitmaps on dispose and drops late results", async () => {
    const { load, pending } = deferredLoader();
    const close = vi.fn();
    const strip = new ThumbnailStrip({
      count: 3,
      load,
      decode: async () => ({ close }) as unknown as CanvasImageSource,
      onReady: () => undefined,
    });
    strip.get(0);
    pending[0]?.resolve(pixels());
    await flush();
    strip.dispose();
    expect(close).toHaveBeenCalledTimes(1);
    expect(strip.get(0)).toBeNull();
  });
});

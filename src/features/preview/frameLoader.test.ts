import { describe, expect, it, vi } from "vitest";
import { type FrameRequest, LatestFrameLoader } from "./frameLoader";

interface Pending {
  request: FrameRequest;
  resolve: (buffer: ArrayBuffer) => void;
  reject: (error: unknown) => void;
}

/** A fake backend whose answers arrive only when the test resolves them. */
function fakeBackend() {
  const calls: Pending[] = [];
  const fetch = vi.fn(
    (request: FrameRequest) =>
      new Promise<ArrayBuffer>((resolve, reject) => {
        calls.push({ request, resolve, reject });
      }),
  );
  const onFrame = vi.fn();
  const onError = vi.fn();
  const loader = new LatestFrameLoader({ fetch, onFrame, onError });
  return { calls, fetch, onFrame, onError, loader };
}

const buffer = (tag: number) => new Uint8Array([tag]).buffer;
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

describe("LatestFrameLoader", () => {
  it("sends one request at a time and collapses the waiting ones into the newest", async () => {
    const { calls, fetch, onFrame, loader } = fakeBackend();

    loader.request({ frame: 1, mode: "output" });
    loader.request({ frame: 2, mode: "output" });
    loader.request({ frame: 3, mode: "output" });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(calls[0]?.request.frame).toBe(1);

    calls[0]?.resolve(buffer(1));
    await flush();
    expect(onFrame).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(calls[1]?.request.frame).toBe(3);
  });

  it("never paints an answer older than the one already shown", async () => {
    const { calls, onFrame, loader } = fakeBackend();
    loader.request({ frame: 1, mode: "output" });
    loader.request({ frame: 2, mode: "output" });
    calls[0]?.resolve(buffer(1));
    await flush();
    calls[1]?.resolve(buffer(2));
    await flush();

    expect(onFrame.mock.calls.map(([, request]) => request.frame)).toEqual([1, 2]);
  });

  it("ignores answers that arrive after a reset", async () => {
    const { calls, onFrame, loader } = fakeBackend();
    loader.request({ frame: 7, mode: "source", sourceTime: 1, maxWidth: 800, maxHeight: 450 });
    loader.reset();
    calls[0]?.resolve(buffer(7));
    await flush();

    expect(onFrame).not.toHaveBeenCalled();
  });

  it("drops a pending request on reset, so nothing stale is sent", async () => {
    const { calls, fetch, loader } = fakeBackend();
    loader.request({ frame: 1, mode: "output" });
    loader.request({ frame: 2, mode: "output" });
    loader.reset();
    calls[0]?.resolve(buffer(1));
    await flush();

    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("reports failures to onError and keeps serving later requests", async () => {
    const { calls, onError, onFrame, loader } = fakeBackend();
    loader.request({ frame: 1, mode: "output" });
    calls[0]?.reject(new Error("decode"));
    await flush();
    expect(onError).toHaveBeenCalledTimes(1);

    loader.request({ frame: 2, mode: "output" });
    calls[1]?.resolve(buffer(2));
    await flush();
    expect(onFrame).toHaveBeenCalledTimes(1);
  });

  it("accepts requests again after start, as an effect that runs twice needs", async () => {
    const { fetch, onFrame, calls, loader } = fakeBackend();
    loader.dispose();
    loader.start();
    loader.request({ frame: 4, mode: "output" });
    calls[0]?.resolve(buffer(4));
    await flush();

    expect(fetch).toHaveBeenCalledTimes(1);
    expect(onFrame).toHaveBeenCalledTimes(1);
  });

  it("stops for good after dispose", async () => {
    const { fetch, onFrame, loader } = fakeBackend();
    loader.dispose();
    loader.request({ frame: 1, mode: "output" });
    await flush();

    expect(fetch).not.toHaveBeenCalled();
    expect(onFrame).not.toHaveBeenCalled();
  });
});

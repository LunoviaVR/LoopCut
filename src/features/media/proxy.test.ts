import "@/ui/test/env";
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import { mediaProxyStart } from "@/ipc/commands";
import { resetProxyStates, startProxy, useProxyState } from "./proxy";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return { ...actual, mediaProxyStart: vi.fn() };
});

beforeEach(() => {
  vi.resetAllMocks();
  resetProxyStates();
});

describe("proxy outcome", () => {
  it("reports a failure that arrives on the channel, even though the command resolved", async () => {
    vi.mocked(mediaProxyStart).mockImplementation(async (_token, onProgress) => {
      onProgress({ kind: "error", error: { code: "encode_failed", message: "Encoder refused" } });
    });

    startProxy(sampleMedia({ token: "tok-channel-error", needsProxy: true }));
    const { result } = renderHook(() => useProxyState("tok-channel-error"));

    await waitFor(() =>
      expect(result.current).toEqual({ status: "failed", message: "Encoder refused" }),
    );
  });

  it("becomes ready with the url the done event carries", async () => {
    vi.mocked(mediaProxyStart).mockImplementation(async (_token, onProgress) => {
      onProgress({ kind: "progress", value: 0.5 });
      onProgress({ kind: "done", url: "http://loopcut-media.localhost/proxy/tok-done" });
    });

    startProxy(sampleMedia({ token: "tok-done", needsProxy: true }));
    const { result } = renderHook(() => useProxyState("tok-done"));

    await waitFor(() =>
      expect(result.current).toEqual({
        status: "ready",
        url: "http://loopcut-media.localhost/proxy/tok-done",
      }),
    );
  });

  it("does not call a run ready when it resolved without any result and has no proxy", async () => {
    vi.mocked(mediaProxyStart).mockResolvedValue(undefined);

    startProxy(sampleMedia({ token: "tok-silent", needsProxy: true, proxyUrl: null }));
    const { result } = renderHook(() => useProxyState("tok-silent"));

    await waitFor(() => expect(result.current.status).toBe("failed"));
    expect(result.current).toEqual({
      status: "failed",
      message: "The preview copy did not finish",
    });
  });

  it("uses an existing proxy when the run resolved without a result", async () => {
    vi.mocked(mediaProxyStart).mockResolvedValue(undefined);

    startProxy(
      sampleMedia({
        token: "tok-existing",
        needsProxy: true,
        proxyUrl: "http://loopcut-media.localhost/proxy/tok-existing",
      }),
    );
    const { result } = renderHook(() => useProxyState("tok-existing"));

    await waitFor(() =>
      expect(result.current).toEqual({
        status: "ready",
        url: "http://loopcut-media.localhost/proxy/tok-existing",
      }),
    );
  });
});

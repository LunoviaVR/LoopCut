import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "@/ipc/bindings/Project";
import { exportEstimate } from "@/ipc/commands";
import type { SizeEstimate } from "@/ipc/types";
import { sampleProject } from "@/state/test-fixtures";
import {
  createEstimateScheduler,
  ESTIMATE_DEBOUNCE_MS,
  type EstimateState,
  requestEstimate,
  resetEstimateState,
  useEstimateStore,
} from "./estimateSync";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    exportEstimate: vi.fn(async () => ({
      bytes: 777,
      low: 677,
      high: 877,
      frames: 10,
      basis: "sample" as const,
    })),
  };
});

const sample = (bytes: number): SizeEstimate => ({
  bytes,
  low: bytes - 100,
  high: bytes + 100,
  frames: 10,
  basis: "sample",
});

/** A promise the test resolves by hand, to control when an estimate returns. */
function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("asking for the estimate again", () => {
  it("runs the estimate of the open project after the debounce and publishes it", async () => {
    resetEstimateState();
    requestEstimate(sampleProject({ name: "retry" }));
    expect(useEstimateStore.getState().pending).toBe(true);

    await vi.advanceTimersByTimeAsync(ESTIMATE_DEBOUNCE_MS);

    expect(exportEstimate).toHaveBeenCalledWith(expect.objectContaining({ name: "retry" }));
    expect(useEstimateStore.getState()).toMatchObject({
      estimate: { bytes: 777 },
      error: null,
      pending: false,
    });
  });
});

describe("estimate scheduler", () => {
  it("waits for the debounce and asks once for the latest project", async () => {
    const estimate = vi.fn(async (_project: Project) => sample(1000));
    const states: EstimateState[] = [];
    const scheduler = createEstimateScheduler({ estimate, onState: (state) => states.push(state) });

    scheduler.schedule(sampleProject({ name: "one" }));
    vi.advanceTimersByTime(300);
    scheduler.schedule(sampleProject({ name: "two" }));
    vi.advanceTimersByTime(599);
    expect(estimate).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(estimate).toHaveBeenCalledTimes(1);
    expect(estimate.mock.calls[0]?.[0].name).toBe("two");

    await vi.runAllTimersAsync();
    expect(states.at(-1)).toEqual({ estimate: sample(1000), error: null, pending: false });
  });

  it("marks the estimate pending at once and keeps the last good estimate visible", async () => {
    const first = deferred<SizeEstimate>();
    const estimate = vi
      .fn<(project: unknown) => Promise<SizeEstimate>>()
      .mockResolvedValueOnce(sample(1000))
      .mockReturnValueOnce(first.promise);
    const states: EstimateState[] = [];
    const scheduler = createEstimateScheduler({ estimate, onState: (state) => states.push(state) });

    scheduler.schedule(sampleProject());
    await vi.runAllTimersAsync();
    scheduler.schedule(sampleProject({ name: "changed" }));

    expect(states.at(-1)).toEqual({ estimate: sample(1000), error: null, pending: true });
  });

  it("drops a slow estimate that finishes after a newer request started", async () => {
    const slow = deferred<SizeEstimate>();
    const estimate = vi
      .fn<(project: unknown) => Promise<SizeEstimate>>()
      .mockReturnValueOnce(slow.promise)
      .mockResolvedValueOnce(sample(2000));
    const states: EstimateState[] = [];
    const scheduler = createEstimateScheduler({ estimate, onState: (state) => states.push(state) });

    scheduler.schedule(sampleProject({ name: "old" }));
    vi.advanceTimersByTime(600);
    scheduler.schedule(sampleProject({ name: "new" }));
    vi.advanceTimersByTime(600);
    await vi.runAllTimersAsync();
    expect(states.at(-1)?.estimate).toEqual(sample(2000));

    // The old request answers last; its result must not overwrite the newer one.
    slow.resolve(sample(9999));
    await vi.runAllTimersAsync();
    expect(states.at(-1)).toEqual({ estimate: sample(2000), error: null, pending: false });
    expect(states.some((state) => state.estimate?.bytes === 9999)).toBe(false);
  });

  it("reports a failure as the message, with no estimate", async () => {
    const estimate = vi.fn(() => Promise.reject(new Error("encoder missing")));
    const states: EstimateState[] = [];
    const scheduler = createEstimateScheduler({ estimate, onState: (state) => states.push(state) });

    scheduler.schedule(sampleProject());
    await vi.runAllTimersAsync();

    expect(states.at(-1)).toEqual({ estimate: null, error: "encoder missing", pending: false });
  });

  it("cancels a waiting request and ignores a running one", async () => {
    const running = deferred<SizeEstimate>();
    const estimate = vi.fn(() => running.promise);
    const states: EstimateState[] = [];
    const scheduler = createEstimateScheduler({ estimate, onState: (state) => states.push(state) });

    scheduler.schedule(sampleProject());
    scheduler.cancel();
    vi.advanceTimersByTime(1000);
    expect(estimate).not.toHaveBeenCalled();
    expect(states.at(-1)?.pending).toBe(false);

    scheduler.schedule(sampleProject());
    vi.advanceTimersByTime(600);
    scheduler.cancel();
    running.resolve(sample(5));
    await vi.runAllTimersAsync();
    expect(states.at(-1)?.estimate).toBeNull();
    expect(states.at(-1)?.pending).toBe(false);
  });
});

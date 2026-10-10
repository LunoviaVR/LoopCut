import { describe, expect, it } from "vitest";
import { enqueuePreviewTask } from "./backendQueue";

/** A promise that settles when the test says so. */
function gate<T>() {
  let open: (value: T) => void = () => {};
  const promise = new Promise<T>((resolve) => {
    open = resolve;
  });
  return { promise, open };
}

describe("enqueuePreviewTask", () => {
  it("runs tasks one after another, in the order they were queued", async () => {
    const order: string[] = [];
    const first = gate<void>();
    const a = enqueuePreviewTask(async () => {
      order.push("a:start");
      await first.promise;
      order.push("a:end");
      return "a";
    });
    const b = enqueuePreviewTask(async () => {
      order.push("b:start");
      return "b";
    });

    // While `a` is held open, `b` must not start.
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(order).toEqual(["a:start"]);

    first.open();
    await expect(a).resolves.toBe("a");
    await expect(b).resolves.toBe("b");
    expect(order).toEqual(["a:start", "a:end", "b:start"]);
  });

  it("rejects only the failing task and keeps the queue moving", async () => {
    const failing = enqueuePreviewTask(async () => {
      throw new Error("decode failed");
    });
    const next = enqueuePreviewTask(async () => 42);

    await expect(failing).rejects.toThrow("decode failed");
    await expect(next).resolves.toBe(42);
  });
});

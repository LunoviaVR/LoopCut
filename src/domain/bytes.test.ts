import { describe, expect, it } from "vitest";
import { formatBytes, MB_BYTES } from "@/domain/bytes";

describe("formatBytes", () => {
  it("uses decimal units, so one megabyte is 1,000,000 bytes", () => {
    expect(MB_BYTES).toBe(1_000_000);
    expect(formatBytes(1_000_000)).toBe("1.0 MB");
  });

  it("shows plain bytes below one kilobyte", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
  });

  it("shows one decimal place above that", () => {
    expect(formatBytes(1000)).toBe("1.0 kB");
    expect(formatBytes(1500)).toBe("1.5 kB");
    expect(formatBytes(842_300)).toBe("842.3 kB");
    expect(formatBytes(9_876_543)).toBe("9.9 MB");
    expect(formatBytes(1_234_567_890)).toBe("1.2 GB");
  });

  it("promotes values that round up to the next unit", () => {
    expect(formatBytes(999_960)).toBe("1.0 MB");
    expect(formatBytes(999.6)).toBe("1.0 kB");
  });

  it("reads invalid or negative input as zero bytes", () => {
    expect(formatBytes(Number.NaN)).toBe("0 B");
    expect(formatBytes(-10)).toBe("0 B");
    expect(formatBytes(Number.POSITIVE_INFINITY)).toBe("0 B");
  });
});

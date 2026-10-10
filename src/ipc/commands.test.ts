import "@/ui/test/env";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  CommandError,
  hwProbe,
  mediaImport,
  pickVideoFile,
  previewFrame,
  projectNew,
  settingsGet,
  spikeFlags,
} from "./commands";
import { parseFrame } from "./frames";

vi.mock("@tauri-apps/api/core", () => {
  class Channel<T> {
    onmessage: (value: T) => void;
    constructor(onmessage?: (value: T) => void) {
      this.onmessage = onmessage ?? (() => {});
    }
  }
  return { invoke: vi.fn(), isTauri: vi.fn(() => false), Channel };
});

const invokeMock = vi.mocked(invoke);
const isTauriMock = vi.mocked(isTauri);

describe("commands outside Tauri", () => {
  beforeEach(() => {
    isTauriMock.mockReturnValue(false);
    invokeMock.mockReset();
  });

  afterEach(() => {
    isTauriMock.mockReturnValue(false);
  });

  it("routes to the synthetic source and never calls invoke", async () => {
    const media = await mediaImport("C:\\Videos\\ranked-ace.mp4");

    expect(media.fileName).toBe("ranked-ace.mp4");
    expect(media.info.video.width).toBe(1920);
    expect(media.info.video.height).toBe(1080);
    expect(media.info.video.frameRate).toEqual({ num: 30, den: 1 });
    expect(media.info.duration).toBeCloseTo(12.4);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("builds a default project from the imported token", async () => {
    const media = await mediaImport("C:\\Videos\\ranked-ace.mp4");
    const project = await projectNew(media.token);

    expect(project.name).toBe("ranked-ace");
    expect(project.output.fps).toEqual({ num: 20, den: 1 });
    expect(project.output.size).toEqual({ mode: "width", width: 480 });
    expect(project.clips[0]?.segments).toEqual([{ start: 0, end: 6 }]);
  });

  it("refuses a network share until the caller confirms, with a normalized error", async () => {
    const error = await mediaImport("\\\\server\\share\\clip.mp4").catch((e: unknown) => e);

    expect(error).toBeInstanceOf(CommandError);
    expect((error as CommandError).code).toBe("network_path");
    await expect(mediaImport("\\\\server\\share\\clip.mp4", true)).resolves.toMatchObject({
      fileName: "clip.mp4",
    });
  });

  it("rejects an unknown media token with an AppError shape", async () => {
    const error = await projectNew("tok-does-not-exist").catch((e: unknown) => e);

    expect(error).toBeInstanceOf(CommandError);
    expect((error as CommandError).code).toBe("media_not_found");
    expect((error as CommandError).message).toMatch(/no longer open/);
  });

  it("returns preview frames in the 8-byte header format", async () => {
    const media = await mediaImport("C:\\Videos\\ranked-ace.mp4");
    const project = await projectNew(media.token);
    const { previewSetProject } = await import("./mock");
    const info = await previewSetProject(project, { width: 640, height: 360, dpr: 1 });

    const frame = parseFrame(await previewFrame(3, "output"));

    expect(frame.width).toBe(info.renderWidth);
    expect(frame.height).toBe(info.renderHeight);
    expect(frame.rgba.length).toBe(info.renderWidth * info.renderHeight * 4);
  });

  it("streams probe steps through the plain callback and resolves with capabilities", async () => {
    const steps: string[] = [];
    const capabilities = await hwProbe(true, (event) => {
      if (event.kind === "step") steps.push(event.label);
    });

    expect(steps.length).toBeGreaterThan(0);
    const names = capabilities.devices.map((entry) => entry.device.key.name);
    expect(names).toContain("AMD Radeon RX 9060 XT");
    expect(names).toContain("Microsoft Basic Render Driver");
    const amd = capabilities.devices.find((entry) => entry.device.vendor === "amd");
    expect(amd?.encode.find((row) => row.backend === "amf" && row.codec === "hevc")).toMatchObject({
      status: "ok",
      minWidth: 426,
      minHeight: 240,
    });
  });

  it("returns the default settings", async () => {
    const settings = await settingsGet();

    expect(settings.hw.enabled).toBe(true);
    expect(settings.hw.gpu).toEqual({ mode: "auto" });
    expect(settings.cache.maxDiskGb).toBe(10);
  });

  it("opens the mock file picker outside Tauri", async () => {
    await expect(pickVideoFile()).resolves.toMatch(/ranked-ace\.mp4$/);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("commands inside Tauri", () => {
  beforeEach(() => {
    isTauriMock.mockReturnValue(true);
    invokeMock.mockReset();
  });

  afterEach(() => {
    isTauriMock.mockReturnValue(false);
  });

  it("calls the snake_case Rust command with camelCase arguments", async () => {
    invokeMock.mockResolvedValueOnce({ token: "tok-1" });

    await mediaImport("D:\\a.mp4", true);

    expect(invokeMock).toHaveBeenCalledWith("media_import", {
      path: "D:\\a.mp4",
      confirmNetwork: true,
    });
  });

  it("normalizes a rejected AppError object into CommandError", async () => {
    invokeMock.mockRejectedValueOnce({ code: "network_path", message: "network share" });

    const error = await mediaImport("\\\\x\\y.mp4").catch((e: unknown) => e);

    expect(error).toBeInstanceOf(CommandError);
    expect((error as CommandError).code).toBe("network_path");
  });

  it("wraps unknown failures as code unknown", async () => {
    invokeMock.mockRejectedValueOnce("boom");

    const error = await settingsGet().catch((e: unknown) => e);

    expect((error as CommandError).code).toBe("unknown");
    expect((error as CommandError).message).toBe("boom");
  });

  it("reads the spike flag through invoke inside Tauri", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ run: true });

    await expect(spikeFlags()).resolves.toEqual({ run: true });
    expect(invoke).toHaveBeenCalledWith("spike_flags");
  });
});

import "@/ui/test/env";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sampleProject } from "@/state/test-fixtures";
import {
  exportEstimate,
  fetchThumbnail,
  mediaImport,
  mediaThumbnails,
  previewFrame,
  previewSetProject,
  previewStatus,
  sequencePlan,
} from "./commands";
import { parseFrame } from "./frames";

async function openedProject() {
  const media = await mediaImport("C:\\Videos\\ranked-ace.mp4");
  const base = sampleProject();
  const clip = base.clips[0];
  if (!clip) throw new Error("clip");
  return { media, project: { ...base, clips: [{ ...clip, path: media.path }] } };
}

describe("browser mock: preview", () => {
  beforeEach(() => {
    // Only the clock: the mock import waits on a timer that the test does not drive.
    vi.useFakeTimers({ toFake: ["Date"] });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders source frames fitted into the bounds and output frames at the render size", async () => {
    const { project } = await openedProject();
    await previewSetProject(project, { width: 640, height: 360, dpr: 1 });

    const source = parseFrame(await previewFrame(0, "source", 2.5, 400, 300));
    expect(source.width).toBeLessThanOrEqual(400);
    expect(source.height).toBeLessThanOrEqual(300);
    expect(source.width / source.height).toBeCloseTo(16 / 9, 1);

    const output = parseFrame(await previewFrame(3, "output"));
    expect(output.width).toBe(480);
  });

  it("reports decode progress that climbs to 1 and never reports a fallback", async () => {
    const { project } = await openedProject();
    await previewSetProject(project, { width: 640, height: 360, dpr: 1 });

    expect((await previewStatus()).decodeProgress).toBe(0);
    vi.advanceTimersByTime(600);
    const midway = (await previewStatus()).decodeProgress;
    expect(midway).toBeGreaterThan(0);
    expect(midway).toBeLessThan(1);
    vi.advanceTimersByTime(2000);
    const status = await previewStatus();
    expect(status.decodeProgress).toBe(1);
    expect(status.fallbacks).toEqual([]);
  });

  it("gives the same frame count as the sequence summary", async () => {
    const { project } = await openedProject();
    const summary = await sequencePlan(project);
    await previewSetProject(project, { width: 640, height: 360, dpr: 1 });

    expect(summary.frames).toBe(120);
    expect(summary.delaysCs).toHaveLength(120);
  });
});

describe("browser mock: thumbnails", () => {
  it("reports an even height and serves RGBA of exactly that size", async () => {
    const { media } = await openedProject();
    const set = await mediaThumbnails(media.token, 5, 71);
    expect(set.height).toBe(70);
    expect(set.times).toHaveLength(5);

    const pixels = await fetchThumbnail(media.token, set.height, 2);
    expect(pixels.height).toBe(70);
    expect(pixels.rgba.length).toBe(pixels.width * pixels.height * 4);
  });
});

describe("browser mock: documented preview errors and export estimate", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("refuses a frame past the end of the plan with preview_frame_out_of_range", async () => {
    const { project } = await openedProject();
    await previewSetProject(project, { width: 640, height: 360, dpr: 1 });

    await expect(previewFrame(120, "output")).rejects.toMatchObject({
      code: "preview_frame_out_of_range",
    });
  });

  it("refuses a frame before any project is open with preview_no_project", async () => {
    vi.resetModules();
    const fresh = await import("./mock");

    await expect(fresh.previewFrame(0, "output")).rejects.toMatchObject({
      code: "preview_no_project",
    });
  });

  it("scales the estimate with the frame count, the width and the quality", async () => {
    const { project } = await openedProject();
    const base = await exportEstimate(project);
    expect(base.bytes).toBe(3_565_000);
    expect(base.frames).toBe(120);

    const wider = await exportEstimate({
      ...project,
      output: { ...project.output, size: { mode: "width", width: 960 } },
    });
    expect(wider.bytes).toBeCloseTo(base.bytes * 4, -3);

    const lower = await exportEstimate({
      ...project,
      export: { ...project.export, quality: 20 },
    });
    expect(lower.bytes).toBeLessThan(base.bytes);
    expect(lower.low).toBeLessThan(lower.bytes);
    expect(lower.high).toBeGreaterThan(lower.bytes);

    const shorter = await exportEstimate({
      ...project,
      clips: [
        {
          ...project.clips[0],
          segments: [{ start: 0, end: 3 }],
        },
      ],
    } as typeof project);
    expect(shorter.frames).toBeLessThan(base.frames);
    expect(shorter.bytes).toBeLessThan(base.bytes);
  });
});

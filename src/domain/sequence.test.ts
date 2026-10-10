import { describe, expect, it } from "vitest";
import type { Project } from "@/ipc/bindings/Project";
import { sampleProject } from "@/state/test-fixtures";
import { buildSequencePlan, MAX_OUTPUT_FRAMES } from "./sequence";
import { sourceTimeAtFrame } from "./timeline";

function withTiming(overrides: Partial<Project["timing"]>): Project {
  const project = sampleProject();
  return { ...project, timing: { ...project.timing, ...overrides } };
}

const timesOf = (project: Project): number[] => {
  const plan = buildSequencePlan(project);
  if (!plan) throw new Error("expected a plan");
  return plan.frames.map((_, index) => sourceTimeAtFrame(plan, index) ?? Number.NaN);
};

describe("buildSequencePlan (mirror of sequencer::plan)", () => {
  it("plays the kept range forward at the output rate, one frame per 1/fps of output", () => {
    const plan = buildSequencePlan(sampleProject());
    expect(plan?.frames).toHaveLength(120);
    expect(plan?.duration).toBeCloseTo(6);
    const times = timesOf(sampleProject());
    expect(times[0]).toBeCloseTo(0);
    expect(times[1]).toBeCloseTo(0.05);
    expect(times[119]).toBeCloseTo(5.95);
  });

  it("reverses the samples for reverse playback", () => {
    const times = timesOf(withTiming({ direction: "reverse" }));
    expect(times[0]).toBeCloseTo(5.95);
    expect(times[119]).toBeCloseTo(0);
  });

  it("ping-pong plays forward, then back without repeating the end frames", () => {
    const plan = buildSequencePlan(withTiming({ direction: "pingPong" }));
    expect(plan?.frames).toHaveLength(120 + 118);
    const times = timesOf(withTiming({ direction: "pingPong" }));
    expect(times[120]).toBeCloseTo(times[118] ?? Number.NaN);
  });

  it("speed divides the output length and walks the source faster", () => {
    const project = withTiming({ speed: 2 });
    const plan = buildSequencePlan(project);
    expect(plan?.frames).toHaveLength(60);
    expect(timesOf(project)[59]).toBeCloseTo(5.9);
  });

  it("blends a crossfade into the start of the loop, up to half the output", () => {
    const plan = buildSequencePlan(withTiming({ crossfade: 0.5 }));
    expect(plan?.frames).toHaveLength(110);
    expect(plan?.frames[0]?.sample.kind).toBe("blend");
    const first = plan?.frames[0]?.sample;
    if (first?.kind !== "blend") throw new Error("expected a blend");
    expect(first.weightB).toBeCloseTo(1 / 11);
  });

  it("holds a frozen source frame for the hold duration", () => {
    const project = sampleProject();
    const clip = project.clips[0];
    if (!clip) throw new Error("clip");
    const held = { ...project, clips: [{ ...clip, holds: [{ at: 3, duration: 0.5 }] }] };
    const plan = buildSequencePlan(held);
    expect(plan?.frames).toHaveLength(130);
    const times = timesOf(held);
    for (let frame = 60; frame <= 69; frame++) expect(times[frame]).toBeCloseTo(3);
    expect(times[70]).toBeCloseTo(3);
    expect(times[71]).toBeCloseTo(3.05);
  });

  it("gives every frame a GIF delay and keeps the total duration in centiseconds", () => {
    const plan = buildSequencePlan(sampleProject());
    expect(plan?.frames.every((frame) => frame.delayCs === 5)).toBe(true);
  });

  it("refuses an invalid segment, an invalid rate and a plan longer than the frame cap", () => {
    const project = sampleProject();
    const clip = project.clips[0];
    if (!clip) throw new Error("clip");
    expect(
      buildSequencePlan({ ...project, clips: [{ ...clip, segments: [{ start: 3, end: 3 }] }] }),
    ).toBeNull();
    expect(
      buildSequencePlan({ ...project, output: { ...project.output, fps: { num: 0, den: 1 } } }),
    ).toBeNull();
    const long = {
      ...project,
      output: { ...project.output, fps: { num: 240, den: 1 } },
      clips: [{ ...clip, segments: [{ start: 0, end: MAX_OUTPUT_FRAMES / 240 + 1 }] }],
    };
    expect(buildSequencePlan(long)).toBeNull();
  });
});

import "@/ui/test/env";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import { resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { aspectForPreset, aspectPresetId, CropPanel, screenRotation } from "./CropPanel";

const ASYMMETRIC = { x: 0.1, y: 0.2, w: 0.3, h: 0.4 };

beforeEach(() => {
  resetEditorState();
  const base = sampleProject();
  useEditorStore.getState().loadProject({
    ...base,
    transform: { ...base.transform, crop: ASYMMETRIC },
  });
});

/** Subscribes to the store the way the inspector does, so every edit re-renders the panel. */
function Harness() {
  const project = useEditorStore((state) => state.project);
  if (!project) return null;
  return <CropPanel media={sampleMedia()} project={project} />;
}

const transform = () => useEditorStore.getState().project?.transform;

describe("CropPanel rotation and flips", () => {
  it("turns the picture and keeps the same content inside the crop, as one undo step", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Rotate right" }));

    expect(transform()?.rotation).toBe(90);
    expect(transform()?.crop.x).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.1, 9);
    expect(transform()?.crop.w).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.h).toBeCloseTo(0.3, 9);

    undo();
    expect(transform()?.rotation).toBe(0);
    expect(transform()?.crop.x).toBeCloseTo(0.1, 9);
    expect(transform()?.crop.w).toBeCloseTo(0.3, 9);
  });

  it("mirrors the crop when the picture is flipped", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Flip horizontal" }));

    expect(transform()?.flipH).toBe(true);
    expect(transform()?.crop.x).toBeCloseTo(0.6, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.2, 9);
  });
});

describe("CropPanel rotation after flips", () => {
  // ASYMMETRIC is {x 0.1, y 0.2, w 0.3, h 0.4}. Rotation is stored before the flips, so one flip reverses the stored
  // turn: Rotate right stores 270 and still turns the picture clockwise on screen. The crop follows the picture, so it
  // always takes the plain clockwise turn. With both flips (or none) the stored turn is the plain quarter turn.
  it("stores the reversed turn after a horizontal flip and turns the crop clockwise", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Flip horizontal" }));
    await userEvent.click(screen.getByRole("button", { name: "Rotate right" }));

    expect(transform()?.rotation).toBe(270);
    expect(transform()?.crop.x).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.6, 9);
    expect(transform()?.crop.w).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.h).toBeCloseTo(0.3, 9);
    // The readout shows the turn seen on screen, not the stored one.
    expect(screen.getByText("90°")).toBeInTheDocument();
  });

  it("stores the reversed turn after a vertical flip and turns the crop clockwise", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Flip vertical" }));
    await userEvent.click(screen.getByRole("button", { name: "Rotate right" }));

    expect(transform()?.rotation).toBe(270);
    expect(transform()?.crop.x).toBeCloseTo(0.2, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.1, 9);
    expect(transform()?.crop.w).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.h).toBeCloseTo(0.3, 9);
  });

  it("uses the plain quarter turn when both axes are flipped", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Flip horizontal" }));
    await userEvent.click(screen.getByRole("button", { name: "Flip vertical" }));
    await userEvent.click(screen.getByRole("button", { name: "Rotate right" }));

    expect(transform()?.rotation).toBe(90);
    expect(transform()?.crop.x).toBeCloseTo(0.2, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.6, 9);
    expect(transform()?.crop.w).toBeCloseTo(0.4, 9);
    expect(transform()?.crop.h).toBeCloseTo(0.3, 9);
  });

  it("stores the plain turn for Rotate left after one flip, so the picture turns counter-clockwise", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Flip horizontal" }));
    await userEvent.click(screen.getByRole("button", { name: "Rotate left" }));

    expect(transform()?.rotation).toBe(90);
    expect(transform()?.crop.x).toBeCloseTo(0.2, 9);
    expect(transform()?.crop.y).toBeCloseTo(0.1, 9);
  });
});

describe("CropPanel fields and reset", () => {
  it("sets the X field in display pixels", async () => {
    render(<Harness />);
    const x = screen.getByRole("textbox", { name: "X" });
    expect(x).toHaveValue("192");

    await userEvent.clear(x);
    await userEvent.type(x, "384{Enter}");

    expect(transform()?.crop.x).toBeCloseTo(384 / 1920, 9);
  });

  it("resets the crop to the whole frame", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Reset crop" }));

    expect(transform()?.crop).toEqual({ x: 0, y: 0, w: 1, h: 1 });
  });
});

describe("aspect presets", () => {
  it("names the stored aspect, and turns a preset into a ratio in lowest terms", () => {
    const size = { width: 1920, height: 1080 };
    expect(aspectPresetId(null, size)).toBe("free");
    expect(aspectPresetId({ num: 16, den: 9 }, size)).toBe("original");
    expect(aspectPresetId({ num: 9, den: 16 }, size)).toBe("9:16");
    expect(aspectPresetId({ num: 7, den: 3 }, size)).toBe("custom");
    expect(aspectForPreset("4:3", size)).toEqual({ num: 4, den: 3 });
    expect(aspectForPreset("original", size)).toEqual({ num: 16, den: 9 });
    expect(aspectForPreset("free", size)).toBeNull();
  });
});

describe("screenRotation", () => {
  it("reverses the stored turn when exactly one axis is flipped", () => {
    expect(screenRotation({ rotation: 90, flipH: false, flipV: false })).toBe(90);
    expect(screenRotation({ rotation: 270, flipH: true, flipV: false })).toBe(90);
    expect(screenRotation({ rotation: 90, flipH: false, flipV: true })).toBe(270);
    expect(screenRotation({ rotation: 180, flipH: true, flipV: true })).toBe(180);
    expect(screenRotation({ rotation: 0, flipH: true, flipV: false })).toBe(0);
  });
});

import "@/ui/test/env";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CurvePoint } from "@/ipc/bindings/CurvePoint";
import { CurveEditor } from "./CurveEditor";

/** A 200 by 176 plot at the origin, so pointer positions map to plot coordinates by hand. */
const PLOT = {
  left: 0,
  top: 0,
  width: 200,
  height: 176,
  right: 200,
  bottom: 176,
  x: 0,
  y: 0,
  toJSON: () => ({}),
};

afterEach(() => {
  vi.restoreAllMocks();
});

/** The editor with its points held in state, the way the grade holds them. */
function Harness({ commits }: { commits: CurvePoint[][] }) {
  const [points, setPoints] = useState<CurvePoint[]>([]);
  return (
    <CurveEditor
      label="Master"
      points={points}
      onCommit={(next) => {
        commits.push(next);
        setPoints(next);
      }}
    />
  );
}

describe("curve editor", () => {
  it("adds a point with a click and drags it, then commits one curve", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    // x = (110 - 10) / 180 and y = (176 - 88 - 10) / 156: the middle of the plot.
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });

    const committed = commits.at(-1) ?? [];
    expect(committed).toHaveLength(3);
    expect(committed[1]?.x).toBeCloseTo(100 / 180, 9);
    expect(committed[1]?.y).toBeCloseTo(0.5, 9);
    expect(committed[0]).toEqual({ x: 0, y: 0 });
    expect(committed[2]).toEqual({ x: 1, y: 1 });
  });

  it("moves the selected point with the arrow keys and removes it with Delete", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });

    plot.focus();
    await userEvent.keyboard("{ArrowUp}");
    expect(commits.at(-1)?.[1]?.y).toBeCloseTo(0.51, 9);

    await userEvent.keyboard("{Delete}");
    expect(commits.at(-1)).toEqual([]);
    expect(
      screen.getByText(
        "Click to add a point and drag it, or press Insert to add one after the selected point. Double-click or press Delete to remove a point.",
      ),
    ).toBeInTheDocument();
  });

  it("writes the grade once per release, however many moves the drag made", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0 });
    for (let step = 1; step <= 20; step += 1) {
      fireEvent.pointerMove(plot, { clientX: 110 + step, clientY: 88 - step, button: 0 });
    }
    expect(commits).toHaveLength(0);

    fireEvent.pointerUp(plot, { clientX: 130, clientY: 68, button: 0 });
    expect(commits).toHaveLength(1);
    expect(commits[0]?.[1]?.x).toBeCloseTo(120 / 180, 9);
  });

  it("keeps the point a double-click on empty plot added, instead of removing it again", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0, detail: 1 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0, detail: 2 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.doubleClick(plot, { clientX: 110, clientY: 88, button: 0 });

    expect(commits.at(-1)).toHaveLength(3);
  });

  it("removes a point that a later double-click lands on", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0, detail: 1 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });

    // A new click sequence on the point: its first press starts a gesture of its own, so the removal is not kept.
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0, detail: 1 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0, detail: 2 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.doubleClick(plot, { clientX: 110, clientY: 88, button: 0 });

    expect(commits.at(-1)).toEqual([]);
  });

  it("cycles the selected point with PageUp and PageDown and announces it", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });

    plot.focus();
    await userEvent.keyboard("{PageDown}");
    expect(screen.getByText(/^Point 1 of 3: input 0\.00/)).toBeInTheDocument();
    await userEvent.keyboard("{PageUp}{PageUp}");
    expect(screen.getByText(/^Point 3 of 3: input 1\.00/)).toBeInTheDocument();
    expect(commits).toHaveLength(1);
  });

  it("lets arrow keys pass through while no point is selected", () => {
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    const notPrevented = fireEvent.keyDown(plot, { key: "ArrowLeft" });

    expect(notPrevented).toBe(true);
    expect(commits).toHaveLength(0);
  });

  it("never removes an endpoint", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    // Grab the bottom-left endpoint of the identity curve and press Delete.
    fireEvent.pointerDown(plot, { clientX: 10, clientY: 166, button: 0 });
    fireEvent.pointerUp(plot, { clientX: 10, clientY: 166, button: 0 });
    await userEvent.keyboard("{Delete}");

    expect(commits.some((points) => points.length === 0)).toBe(false);
  });

  it("adds a point midway to the next one with Insert, on the curve, and announces it", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    plot.focus();
    await userEvent.keyboard("{PageUp}{Insert}");

    expect(commits).toHaveLength(1);
    expect(commits[0]?.[1]?.x).toBeCloseTo(0.5, 9);
    expect(commits[0]?.[1]?.y).toBeCloseTo(0.5, 6);
    expect(screen.getByText(/^Point 2 of 3: input 0\.50, output 0\.50/)).toBeInTheDocument();
  });

  it("adds with the plus key midway to the left neighbour when the last point is selected", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(PLOT as DOMRect);
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });
    fireEvent.pointerDown(plot, { clientX: 110, clientY: 88, button: 0 });
    fireEvent.pointerUp(plot, { clientX: 110, clientY: 88, button: 0 });

    plot.focus();
    await userEvent.keyboard("{PageUp}+");

    // The middle point is at x = 100 / 180 and the last point at 1: the new point sits midway between them.
    const added = commits.at(-1) ?? [];
    expect(added).toHaveLength(4);
    expect(added[2]?.x).toBeCloseTo((100 / 180 + 1) / 2, 9);
    expect(screen.getByText(/^Point 3 of 4:/)).toBeInTheDocument();
  });

  it("does nothing on Insert when no point is selected", () => {
    const commits: CurvePoint[][] = [];
    render(<Harness commits={commits} />);
    const plot = screen.getByRole("application", { name: "Master curve" });

    const notPrevented = fireEvent.keyDown(plot, { key: "Insert" });

    expect(notPrevented).toBe(true);
    expect(commits).toHaveLength(0);
  });
});

import "@/ui/test/env";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { ColorPanel } from "./ColorPanel";
import { resetCompareState, useCompareStore } from "./compareStore";

beforeEach(() => {
  resetEditorState();
  resetCompareState();
  useEditorStore.getState().loadProject(sampleProject());
});

const grade = () => useEditorStore.getState().project?.grade;

describe("color panel sections", () => {
  it("shows the grade, presets, basic controls, wheels, curves, hue bands, LUT and effects", () => {
    render(<ColorPanel />);

    for (const title of [
      "Grade",
      "Presets",
      "Basic",
      "Wheels",
      "Curves",
      "Hue, saturation and luminance",
      "LUT",
      "Effects",
    ]) {
      expect(screen.getByRole("button", { name: title })).toBeInTheDocument();
    }
  });
});

describe("basic controls", () => {
  it("types a value and records it as one undo step", async () => {
    render(<ColorPanel />);
    const exposure = screen.getByRole("textbox", { name: "Exposure" });

    await userEvent.clear(exposure);
    await userEvent.type(exposure, "1.5{Enter}");

    expect(grade()?.exposure).toBe(1.5);
    undo();
    expect(grade()?.exposure).toBe(0);
    expect(canUndo()).toBe(false);
  });

  it("resets a control to its neutral value on a double-click of its label", async () => {
    render(<ColorPanel />);
    const exposure = screen.getByRole("textbox", { name: "Exposure" });
    await userEvent.clear(exposure);
    await userEvent.type(exposure, "2{Enter}");
    expect(grade()?.exposure).toBe(2);

    fireEvent.doubleClick(screen.getByText("Exposure", { selector: "label" }));

    expect(grade()?.exposure).toBe(0);
  });

  it("rejects an out-of-range value and keeps the grade", async () => {
    render(<ColorPanel />);
    const contrast = screen.getByRole("textbox", { name: "Contrast" });

    await userEvent.clear(contrast);
    await userEvent.type(contrast, "3{Enter}");

    expect(grade()?.contrast).toBe(0);
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });
});

describe("bypass and before and after", () => {
  it("turns the grade off with the bypass switch", async () => {
    render(<ColorPanel />);

    await userEvent.click(screen.getByRole("switch", { name: "Bypass grade" }));

    expect(grade()?.enabled).toBe(false);
    await userEvent.click(screen.getByRole("switch", { name: "Bypass grade" }));
    expect(grade()?.enabled).toBe(true);
  });

  it("turns the before and after comparison on and off", async () => {
    render(<ColorPanel />);
    const toggle = screen.getByRole("button", { name: "Before and after" });

    await userEvent.click(toggle);
    expect(useCompareStore.getState().enabled).toBe(true);
    expect(toggle).toHaveAttribute("aria-pressed", "true");
  });
});

describe("presets", () => {
  it("applies a preset: its values, the preset name, and neutral elsewhere", async () => {
    useEditorStore.getState().setGrade({ exposure: 1.5, tint: 0.2 });
    render(<ColorPanel />);

    await userEvent.click(screen.getByRole("button", { name: "Punchy" }));

    expect(grade()?.preset).toBe("punchy");
    expect(grade()?.contrast).toBe(0.25);
    expect(grade()?.exposure).toBe(0);
    expect(grade()?.tint).toBe(0);
    expect(screen.getByRole("button", { name: "Punchy" })).toHaveAttribute("aria-pressed", "true");
    undo();
    expect(grade()?.preset).toBeNull();
  });

  it("un-presses the preset once a basic control is changed by hand", async () => {
    render(<ColorPanel />);
    await userEvent.click(screen.getByRole("button", { name: "Punchy" }));
    const exposure = screen.getByRole("textbox", { name: "Exposure" });

    await userEvent.clear(exposure);
    await userEvent.type(exposure, "1{Enter}");

    expect(screen.getByRole("button", { name: "Punchy" })).toHaveAttribute("aria-pressed", "false");
  });

  it("resets every color control from Reset all", async () => {
    render(<ColorPanel />);
    await userEvent.click(screen.getByRole("button", { name: "Faded" }));

    await userEvent.click(screen.getByRole("button", { name: "Reset all" }));

    expect(grade()?.preset).toBeNull();
    expect(grade()?.contrast).toBe(0);
    expect(grade()?.saturation).toBe(0);
  });
});

describe("preset hint", () => {
  it("says which sliders a preset sets and what it keeps", () => {
    render(<ColorPanel />);

    expect(
      screen.getByText(
        "A preset sets the sliders it names and resets the other Basic and Effects sliders; curves and wheels stay.",
      ),
    ).toBeInTheDocument();
  });
});

describe("hue bands", () => {
  it("shows the sliders of the selected band and writes that band only", async () => {
    render(<ColorPanel />);

    await userEvent.click(screen.getByRole("button", { name: "Blue" }));
    expect(screen.getByText("Band: Blue")).toBeInTheDocument();
    const hue = screen.getByRole("textbox", { name: "Hue" });
    await userEvent.clear(hue);
    await userEvent.type(hue, "20{Enter}");

    expect(grade()?.hsl[5]?.hue).toBe(20);
    expect(grade()?.hsl[0]?.hue).toBe(0);
  });
});

describe("color wheels", () => {
  it("moves the lift puck with the arrow keys and resets it with Home", async () => {
    render(<ColorPanel />);
    const wheel = screen.getByRole("slider", { name: "Lift" });

    wheel.focus();
    await userEvent.keyboard("{ArrowRight}");
    const moved = grade()?.lift ?? [0, 0, 0];
    expect(moved[0]).toBeGreaterThan(moved[1]);
    expect(moved[0]).toBeGreaterThan(moved[2]);
    expect(wheel).toHaveAttribute("aria-valuetext", expect.stringContaining("Hue"));

    await userEvent.keyboard("{Home}");
    expect(grade()?.lift).toEqual([0, 0, 0]);
    expect(wheel).toHaveAttribute("aria-valuetext", "Neutral");
  });

  it("keeps the gamma master level when the puck moves", async () => {
    render(<ColorPanel />);
    const wheel = screen.getByRole("slider", { name: "Gamma" });

    wheel.focus();
    await userEvent.keyboard("{ArrowUp}");

    const gamma = grade()?.gamma ?? [1, 1, 1];
    const level = Math.exp(
      0.2126 * Math.log(gamma[0] ?? 1) +
        0.7152 * Math.log(gamma[1] ?? 1) +
        0.0722 * Math.log(gamma[2] ?? 1),
    );
    expect(level).toBeCloseTo(1, 6);
  });

  it("speaks the master level of a wheel as a number with two decimals", () => {
    render(<ColorPanel />);

    expect(screen.getByRole("slider", { name: "Lift master level" })).toHaveAttribute(
      "aria-valuetext",
      expect.stringMatching(/^-?\d+\.\d{2}$/),
    );
  });

  it("gives the master level a 24px hit area around its 4px track", () => {
    render(<ColorPanel />);

    // The slider root carries the hit area; it is the nearest data-orientation element without a role (the thumb has one).
    const thumb = screen.getByRole("slider", { name: "Lift master level" });
    const root = thumb.closest("[data-orientation]:not([role])");
    expect(root).toHaveAttribute("data-orientation", "vertical");
    expect(root).toHaveClass("w-6");
  });
});

describe("LUT", () => {
  it("loads a .cube file after it is inspected, and removes it again", async () => {
    render(<ColorPanel />);

    await userEvent.click(screen.getByRole("button", { name: "Load .cube file" }));
    await screen.findByText("Demo warm grade");
    expect(grade()?.lutPath).toBe("C:\\LUTs\\demo-warm.cube");
    expect(screen.getByText("33 grid, 3D")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Remove LUT" }));
    await waitFor(() => expect(grade()?.lutPath).toBeNull());
    expect(screen.getByText("No LUT loaded.")).toBeInTheDocument();
  });
});

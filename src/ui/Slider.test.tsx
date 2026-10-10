import "./test/env";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Slider } from "./Slider";

function renderSlider(props: {
  value: number;
  min: number;
  max: number;
  valueText?: (value: number) => string;
}) {
  return render(<Slider step={0.1} aria-label="Exposure" onChange={vi.fn()} {...props} />);
}

describe("Slider fill", () => {
  it("fills from the minimum and draws no zero tick when the range is one-sided", () => {
    renderSlider({ value: 40, min: 0, max: 100 });

    expect(screen.getByTestId("slider-fill")).toHaveStyle({ left: "0%", width: "40%" });
    expect(screen.queryByTestId("slider-zero")).not.toBeInTheDocument();
  });

  it("fills from zero toward a positive value and marks zero in a bipolar range", () => {
    renderSlider({ value: 2, min: -4, max: 4 });

    expect(screen.getByTestId("slider-fill")).toHaveStyle({ left: "50%", width: "25%" });
    expect(screen.getByTestId("slider-zero")).toHaveStyle({ left: "50%" });
  });

  it("fills from zero toward a negative value, so the bar grows left of the tick", () => {
    renderSlider({ value: -2, min: -4, max: 4 });

    expect(screen.getByTestId("slider-fill")).toHaveStyle({ left: "25%", width: "25%" });
  });

  it("leaves the fill empty at zero, so the thumb sits on the tick", () => {
    renderSlider({ value: 0, min: -1, max: 1 });

    expect(screen.getByTestId("slider-fill")).toHaveStyle({ left: "50%", width: "0%" });
  });

  it("fills upward from zero in a vertical bipolar slider, such as a colour wheel's master", () => {
    render(
      <Slider
        step={0.1}
        aria-label="Lift"
        orientation="vertical"
        value={2}
        min={-4}
        max={4}
        onChange={vi.fn()}
      />,
    );

    expect(screen.getByTestId("slider-fill")).toHaveStyle({ bottom: "50%", height: "25%" });
    expect(screen.getByTestId("slider-zero")).toHaveStyle({ bottom: "50%" });
  });

  it("uses the neutral fill, keeping the accent for focus, selection and action", () => {
    renderSlider({ value: 40, min: 0, max: 100 });

    expect(screen.getByTestId("slider-fill")).toHaveClass("bg-fg-muted");
    expect(screen.getByTestId("slider-fill")).not.toHaveClass("bg-accent");
  });
});

describe("Slider under forced colors", () => {
  it("outlines the track and the thumb, where High Contrast drops their fills", () => {
    renderSlider({ value: 40, min: 0, max: 100 });

    expect(screen.getByTestId("slider-fill").parentElement).toHaveClass("forced-colors:outline");
    expect(screen.getByRole("slider")).toHaveClass("forced-colors:outline-2");
  });
});

describe("Slider hit area and labels", () => {
  it("gives a horizontal slider a 24px hit height around the 4px track", () => {
    const { container } = renderSlider({ value: 0, min: 0, max: 1 });

    expect(container.firstElementChild).toHaveClass("h-6");
    expect(container.querySelector(".h-1")).toBeInTheDocument();
  });

  it("speaks the value text on the thumb", () => {
    renderSlider({ value: 1, min: 0, max: 4, valueText: (v) => `${v.toFixed(2)} x` });

    expect(screen.getByRole("slider", { name: "Exposure" })).toHaveAttribute(
      "aria-valuetext",
      "1.00 x",
    );
  });
});

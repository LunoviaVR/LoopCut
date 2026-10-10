import "./test/env";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { NumberField } from "./NumberField";
import { SliderField } from "./SliderField";

function Harness(props: {
  initial: number;
  onCommit?: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
}) {
  const [value, setValue] = useState(props.initial);
  return (
    <NumberField
      label="Speed"
      unit="x"
      value={value}
      min={props.min ?? 0.25}
      max={props.max ?? 4}
      step={props.step ?? 0.05}
      onChange={setValue}
      onCommit={props.onCommit}
    />
  );
}

describe("NumberField", () => {
  it("names the input by its label and shows the unit", () => {
    render(<Harness initial={1} />);

    const input = screen.getByRole("textbox", { name: "Speed" });
    expect(input).toHaveValue("1.00");
    expect(screen.getByText("x")).toBeInTheDocument();
  });

  it("applies a typed value on Enter and commits it once", async () => {
    const onCommit = vi.fn();
    render(<Harness initial={1} onCommit={onCommit} />);

    const input = screen.getByRole("textbox", { name: "Speed" });
    await userEvent.clear(input);
    await userEvent.type(input, "2.5{Enter}");

    expect(input).toHaveValue("2.50");
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(2.5);
  });

  it("steps by one step with the arrow keys and by ten with Shift", async () => {
    const onCommit = vi.fn();
    render(<Harness initial={1} onCommit={onCommit} />);

    const input = screen.getByRole("textbox", { name: "Speed" });
    input.focus();
    await userEvent.keyboard("{ArrowUp}");
    expect(input).toHaveValue("1.05");
    await userEvent.keyboard("{Shift>}{ArrowDown}{/Shift}");
    expect(input).toHaveValue("0.55");
    expect(onCommit).toHaveBeenCalledTimes(2);
  });

  it("clamps to the range instead of overshooting", async () => {
    render(<Harness initial={3.98} />);

    const input = screen.getByRole("textbox", { name: "Speed" });
    input.focus();
    await userEvent.keyboard("{Shift>}{ArrowUp}{/Shift}");

    expect(input).toHaveValue("4.00");
  });

  it("shows a visible error and does not apply an out-of-range value", async () => {
    const onChange = vi.fn();
    render(
      <NumberField label="Speed" value={1} min={0.25} max={4} step={0.05} onChange={onChange} />,
    );

    const input = screen.getByRole("textbox", { name: "Speed" });
    await userEvent.clear(input);
    await userEvent.type(input, "9{Enter}");

    expect(onChange).not.toHaveBeenCalled();
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("alert")).toHaveTextContent("Enter a number from 0.25 to 4.00");
  });

  it("rejects text that is not a number and restores the value on Escape", async () => {
    render(<Harness initial={1} />);

    const input = screen.getByRole("textbox", { name: "Speed" });
    await userEvent.clear(input);
    await userEvent.type(input, "fast{Enter}");
    expect(screen.getByRole("alert")).toHaveTextContent("Enter a number");

    await userEvent.keyboard("{Escape}");
    expect(input).toHaveValue("1.00");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("scrubs by dragging the label and commits once at the end", () => {
    const onChange = vi.fn();
    const onCommit = vi.fn();
    render(
      <NumberField
        label="Speed"
        value={1}
        min={0}
        max={10}
        step={1}
        onChange={onChange}
        onCommit={onCommit}
      />,
    );

    const label = screen.getByText("Speed");
    fireEvent.pointerDown(label, { clientX: 100, button: 0, pointerId: 1 });
    fireEvent.pointerMove(label, { clientX: 108, pointerId: 1 });
    fireEvent.pointerMove(label, { clientX: 112, pointerId: 1 });
    fireEvent.pointerUp(label, { clientX: 112, pointerId: 1 });

    expect(onChange).toHaveBeenLastCalledWith(4);
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(4);
  });

  it("does not commit a drag that ends where it started", () => {
    const onCommit = vi.fn();
    render(
      <NumberField
        label="Speed"
        value={1}
        min={0}
        max={10}
        step={1}
        onChange={vi.fn()}
        onCommit={onCommit}
      />,
    );

    const label = screen.getByText("Speed");
    fireEvent.pointerDown(label, { clientX: 50, button: 0, pointerId: 1 });
    fireEvent.pointerUp(label, { clientX: 50, pointerId: 1 });

    expect(onCommit).not.toHaveBeenCalled();
  });
});

describe("SliderField", () => {
  it("resets to the default when the label is double-clicked, as one committed change", async () => {
    const onChange = vi.fn();
    const onCommit = vi.fn();
    render(
      <SliderField
        label="Exposure"
        value={0.7}
        min={-2}
        max={2}
        step={0.1}
        defaultValue={0}
        onChange={onChange}
        onCommit={onCommit}
      />,
    );

    await userEvent.dblClick(screen.getByText("Exposure"));

    expect(onChange).toHaveBeenLastCalledWith(0);
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(0);
  });

  it("exposes the slider thumb with its value and steps it from the keyboard", async () => {
    const onChange = vi.fn();
    const onCommit = vi.fn();
    render(
      <SliderField
        label="Saturation"
        value={0}
        min={-1}
        max={1}
        step={0.1}
        defaultValue={0}
        onChange={onChange}
        onCommit={onCommit}
      />,
    );

    const thumb = screen.getByRole("slider", { name: "Saturation" });
    expect(thumb).toHaveAttribute("aria-valuenow", "0");
    thumb.focus();
    await userEvent.keyboard("{ArrowRight}");

    expect(onChange).toHaveBeenLastCalledWith(0.1);
    expect(onCommit).toHaveBeenLastCalledWith(0.1);
  });

  it("speaks the value with its digits and unit on the thumb", () => {
    render(
      <SliderField
        label="Speed"
        value={1}
        min={0.25}
        max={4}
        step={0.05}
        unit="x"
        defaultValue={1}
        onChange={vi.fn()}
        onCommit={vi.fn()}
      />,
    );

    expect(screen.getByRole("slider", { name: "Speed" })).toHaveAttribute(
      "aria-valuetext",
      "1.00 x",
    );
  });
});

describe("NumberField layout", () => {
  it("sizes the value box from the width prop, medium by default", () => {
    const { rerender } = render(<NumberField label="Width" value={1080} onChange={vi.fn()} />);
    const box = () => screen.getByRole("textbox", { name: "Width" }).parentElement;

    expect(box()).toHaveClass("w-20");
    rerender(<NumberField label="Width" value={1080} width="sm" onChange={vi.fn()} />);
    expect(box()).toHaveClass("w-16");
    rerender(<NumberField label="Width" value={1080} width="lg" onChange={vi.fn()} />);
    expect(box()).toHaveClass("w-24");
  });

  it("puts the label and the value box on the shared 7rem column", () => {
    render(<NumberField label="Speed" value={1} onChange={vi.fn()} />);

    expect(screen.getByText("Speed").parentElement).toHaveClass("grid-cols-[7rem_minmax(0,1fr)]");
    expect(screen.getByRole("textbox", { name: "Speed" }).parentElement).toHaveClass(
      "justify-self-start",
    );
  });

  it("shows a 2px accent focus ring on the value box and a control-token edge at rest", () => {
    render(<NumberField label="Speed" value={1} onChange={vi.fn()} />);

    expect(screen.getByRole("textbox", { name: "Speed" }).parentElement).toHaveClass(
      "focus-within:outline-2",
      "focus-within:outline-accent",
      "border-line-control",
      "hover:border-fg-muted",
    );
  });
});

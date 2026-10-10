import "@/ui/test/env";
import { act, render, screen, waitFor } from "@testing-library/react";
import { X } from "lucide-react";
import { describe, expect, it } from "vitest";
import { Button } from "./Button";
import { IconButton } from "./IconButton";
import { menuContentClass, menuItemClass } from "./menu";
import { ProgressBar } from "./ProgressBar";
import { Spinner } from "./Spinner";
import { selectedChipClass, selectedFillClass } from "./selected";
import { Toaster, toast } from "./Toaster";

describe("danger button", () => {
  it("is a solid destructive fill with app-coloured text", () => {
    render(<Button variant="danger">Clear cache</Button>);
    const button = screen.getByRole("button", { name: "Clear cache" });

    expect(button.className).toContain("bg-danger");
    expect(button.className).toContain("text-app");
    expect(button.className).not.toContain("border-danger");
  });
});

describe("reduced motion", () => {
  it("stops the spinner under prefers-reduced-motion", () => {
    const { container } = render(<Spinner label="Working" />);
    expect(container.querySelector("svg")?.getAttribute("class")).toContain(
      "motion-reduce:animate-none",
    );
  });

  it("stops the indeterminate progress segment under prefers-reduced-motion", () => {
    const { container } = render(<ProgressBar label="Working" indeterminate />);
    expect(container.querySelector('[role="progressbar"] > div')?.className).toContain(
      "motion-reduce:animate-none",
    );
  });
});

describe("disabled icon buttons", () => {
  it("keep a wrapper that takes the hover, so a disabled control still explains itself", () => {
    render(<IconButton icon={X} aria-label="Undo" shortcut="Ctrl+Z" disabled />);
    const button = screen.getByRole("button", { name: "Undo" });

    expect(button).toBeDisabled();
    expect(button.parentElement?.tagName).toBe("SPAN");
  });

  it("does not remount the button when it becomes disabled", () => {
    const { rerender } = render(<IconButton icon={X} aria-label="Play" />);
    const before = screen.getByRole("button", { name: "Play" });

    rerender(<IconButton icon={X} aria-label="Play" disabled />);

    expect(screen.getByRole("button", { name: "Play" })).toBe(before);
  });
});

describe("pressed toggles", () => {
  it("shows the pressed state with the shared selected look", () => {
    render(<IconButton icon={X} aria-label="Loop" pressed tooltip={false} />);

    expect(screen.getByRole("button", { name: "Loop" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "Loop" })).toHaveClass(
      "aria-pressed:bg-accent-soft",
      "aria-pressed:text-fg",
      "aria-pressed:shadow-[inset_0_0_0_1px_var(--color-accent)]",
    );
  });
});

describe("menu classes", () => {
  it("gives menus the popover shadow, overlay fill and 4px item radius", () => {
    expect(menuContentClass).toContain("shadow-popover");
    expect(menuContentClass).toContain("bg-overlay");
    expect(menuItemClass).toContain("rounded-chip");
  });

  it("highlights the keyboard row with the accent wash, not the hover fill that vanished on the overlay", () => {
    expect(menuItemClass).toContain("data-[highlighted]:bg-accent-soft");
    expect(menuItemClass).not.toContain("data-[highlighted]:bg-hover");
    expect(menuItemClass).toContain("forced-colors:data-[highlighted]:outline-2");
  });

  it("exports the selected look for conditional classes", () => {
    expect(selectedFillClass).toContain("bg-accent-soft");
    expect(selectedFillClass).toContain("shadow-[inset_0_0_0_1px_var(--color-accent)]");
    expect(selectedChipClass).toContain("aria-pressed:bg-accent-soft");
  });
});

describe("toast live regions", () => {
  it("announces problems assertively and notices politely", async () => {
    render(<Toaster />);

    act(() => {
      toast({ title: "Could not open this file", variant: "error" });
      toast({ title: "Export finished", variant: "success" });
    });

    await waitFor(() => {
      const assertive = document.querySelector('[role="status"][aria-live="assertive"]');
      expect(assertive?.textContent).toContain("Could not open this file");
    });
    const polite = document.querySelector('[role="status"][aria-live="polite"]');
    expect(polite?.textContent).toContain("Export finished");
  });
});

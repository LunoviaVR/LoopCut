import "./test/env";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Undo2 } from "lucide-react";
import { describe, expect, it, vi } from "vitest";
import { Button } from "./Button";
import { IconButton } from "./IconButton";
import { Kbd } from "./Kbd";
import { Spinner } from "./Spinner";

describe("Button", () => {
  it("fires onClick and defaults to type=button", async () => {
    const onClick = vi.fn();
    render(<Button onClick={onClick}>Export</Button>);

    const button = screen.getByRole("button", { name: "Export" });
    expect(button).toHaveAttribute("type", "button");
    await userEvent.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("does not fire when disabled", async () => {
    const onClick = vi.fn();
    render(
      <Button disabled onClick={onClick}>
        Export
      </Button>,
    );

    await userEvent.click(screen.getByRole("button", { name: "Export" }));
    expect(onClick).not.toHaveBeenCalled();
  });

  it("shows a status spinner, blocks activation and marks itself busy while loading", async () => {
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Saving
      </Button>,
    );

    const button = screen.getByRole("button", { name: "Saving" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");
    await userEvent.click(button);
    expect(onClick).not.toHaveBeenCalled();
  });

  it("renders a trailing hint after the label", () => {
    render(<Button trailing={<span data-testid="hint">Ctrl+E</span>}>Export</Button>);

    expect(screen.getByRole("button", { name: /Export/ })).toContainElement(
      screen.getByTestId("hint"),
    );
  });
});

describe("Spinner", () => {
  it("is decorative without a label and a status with one", () => {
    const { rerender } = render(<Spinner />);
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    rerender(<Spinner label="Loading media" />);
    expect(screen.getByRole("status", { name: "Loading media" })).toBeInTheDocument();
  });
});

describe("IconButton", () => {
  it("is named by its required aria-label", () => {
    render(<IconButton icon={Undo2} aria-label="Undo" tooltip={false} />);

    expect(screen.getByRole("button", { name: "Undo" })).toBeInTheDocument();
  });

  it("reports a toggle state through aria-pressed", () => {
    render(<IconButton icon={Undo2} aria-label="Inspector" pressed tooltip={false} />);

    expect(screen.getByRole("button", { name: "Inspector" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("shows its name and shortcut in a tooltip when focused", async () => {
    render(<IconButton icon={Undo2} aria-label="Undo" shortcut="Ctrl+Z" />);

    await userEvent.tab();
    expect(screen.getByRole("button", { name: "Undo" })).toHaveFocus();
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 2000 });
    await waitFor(() => expect(tooltip).toHaveTextContent("Undo"));
    expect(tooltip).toHaveTextContent("Ctrl");
    expect(tooltip).toHaveTextContent("Z");
  });
});

describe("Kbd", () => {
  it("splits a shortcut into key caps", () => {
    const { container } = render(<Kbd keys="Ctrl+Shift+Z" />);

    const caps = Array.from(container.querySelectorAll("kbd")).map((cap) => cap.textContent);
    expect(caps).toEqual(["Ctrl", "Shift", "Z"]);
  });
});

import "./test/env";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Button } from "./Button";
import { Dialog } from "./Dialog";
import { Toaster, toast } from "./Toaster";

describe("Dialog", () => {
  it("names the dialog by its title and describes it", () => {
    render(
      <Dialog
        open
        onOpenChange={vi.fn()}
        title="Discard changes?"
        description="Unsaved edits will be lost."
      >
        <p>Body</p>
      </Dialog>,
    );

    const dialog = screen.getByRole("dialog", { name: "Discard changes?" });
    expect(dialog).toHaveAccessibleDescription("Unsaved edits will be lost.");
  });

  it("closes on Escape and on the close button, reporting false both times", async () => {
    const onOpenChange = vi.fn();
    render(
      <Dialog open onOpenChange={onOpenChange} title="Export" description="Start the export?" />,
    );

    await userEvent.keyboard("{Escape}");
    expect(onOpenChange).toHaveBeenLastCalledWith(false);

    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onOpenChange).toHaveBeenCalledTimes(2);
  });

  it("moves focus inside on open and renders footer actions", async () => {
    const onConfirm = vi.fn();
    render(
      <Dialog
        open
        onOpenChange={vi.fn()}
        title="Replace file?"
        footer={
          <Button variant="primary" onClick={onConfirm}>
            Replace
          </Button>
        }
      />,
    );

    await waitFor(() =>
      expect(screen.getByRole("dialog")).toContainElement(document.activeElement as HTMLElement),
    );
    await userEvent.click(screen.getByRole("button", { name: "Replace" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});

describe("floating layers", () => {
  it("casts the shared dialog shadow token, not a literal", () => {
    render(<Dialog open onOpenChange={vi.fn()} title="Export" description="Start the export?" />);

    expect(screen.getByRole("dialog", { name: "Export" })).toHaveClass("shadow-overlay");
  });
});

describe("toast", () => {
  it("shows a title and dismisses on the close button", async () => {
    render(<Toaster />);

    act(() => {
      toast({ title: "Export finished", variant: "success" });
    });
    expect(await screen.findByText("Export finished")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    await waitFor(() => expect(screen.queryByText("Export finished")).not.toBeInTheDocument());
  });

  it("runs its recovery action and closes", async () => {
    const onAction = vi.fn();
    render(<Toaster />);

    act(() => {
      toast({
        title: "This file is on a network share",
        variant: "warn",
        action: { label: "Import anyway", onAction },
      });
    });
    await userEvent.click(await screen.findByRole("button", { name: "Import anyway" }));

    expect(onAction).toHaveBeenCalledTimes(1);
    await waitFor(() =>
      expect(screen.queryByText("This file is on a network share")).not.toBeInTheDocument(),
    );
  });

  it("labels the variant icon for assistive tech", async () => {
    render(<Toaster />);

    act(() => {
      toast({
        title: "Could not open this file",
        variant: "error",
        description: "Check the path.",
      });
    });

    expect(await screen.findByRole("img", { name: "Problem" })).toBeInTheDocument();
    expect(screen.getByText("Check the path.")).toBeInTheDocument();
  });

  it("keeps the info icon neutral so the accent stays with action and selection", async () => {
    render(<Toaster />);

    act(() => {
      toast({ title: "Export queued", variant: "info" });
    });

    const icon = await screen.findByRole("img", { name: "Notice" });
    expect(icon).toHaveClass("text-fg-muted");
    expect(icon).not.toHaveClass("text-accent");
  });
});

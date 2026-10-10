import "./test/env";
import { render, screen } from "@testing-library/react";
import { FolderOpen } from "lucide-react";
import { describe, expect, it } from "vitest";
import { Badge } from "./Badge";
import { EmptyState } from "./EmptyState";
import { Panel } from "./Panel";
import { ProgressBar } from "./ProgressBar";

describe("Badge", () => {
  it("uses the solid accent treatment with graphite text for the accent variant", () => {
    render(<Badge variant="accent">New</Badge>);

    const badge = screen.getByText("New");
    expect(badge).toHaveClass("bg-accent", "text-app");
  });

  it("tints the fill and colors the text for state variants", () => {
    render(<Badge variant="danger">Over the limit</Badge>);

    expect(screen.getByText("Over the limit")).toHaveClass("text-danger");
  });
});

describe("ProgressBar", () => {
  it("exposes a determinate value clamped to 0 to 100", () => {
    const { rerender } = render(<ProgressBar label="Export" value={0.42} />);
    const bar = screen.getByRole("progressbar", { name: "Export" });
    expect(bar).toHaveAttribute("aria-valuenow", "42");
    expect(bar).toHaveAttribute("aria-valuemin", "0");
    expect(bar).toHaveAttribute("aria-valuemax", "100");

    rerender(<ProgressBar label="Export" value={3} />);
    expect(screen.getByRole("progressbar", { name: "Export" })).toHaveAttribute(
      "aria-valuenow",
      "100",
    );
  });

  it("omits the value while indeterminate", () => {
    render(<ProgressBar label="Probing GPUs" indeterminate />);

    expect(screen.getByRole("progressbar", { name: "Probing GPUs" })).not.toHaveAttribute(
      "aria-valuenow",
    );
  });
});

describe("EmptyState", () => {
  it("states what to do with a heading at the requested level and an action", () => {
    render(
      <EmptyState
        icon={FolderOpen}
        title="Drop a video or press Ctrl+O"
        body="Files are read on this PC."
        headingLevel={3}
        action={<button type="button">Open video</button>}
      />,
    );

    expect(
      screen.getByRole("heading", { level: 3, name: "Drop a video or press Ctrl+O" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Files are read on this PC.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open video" })).toBeInTheDocument();
  });
});

describe("Panel", () => {
  it("labels the landmark, titles the header and pins the footer below the body", () => {
    render(
      <Panel as="aside" aria-label="Inspector" title="Trim" footer={<p>Export summary</p>}>
        <p>Speed controls</p>
      </Panel>,
    );

    const landmark = screen.getByRole("complementary", { name: "Inspector" });
    expect(landmark).toContainElement(screen.getByRole("heading", { name: "Trim" }));
    expect(landmark).toContainElement(screen.getByText("Export summary"));
    expect(landmark).toContainElement(screen.getByText("Speed controls"));
  });

  it("titles the tool at 14px semibold and keeps 12px between the last row and the footer", () => {
    render(
      <Panel title="Media" footer={<p>Export summary</p>}>
        <p>Source file</p>
      </Panel>,
    );

    expect(screen.getByRole("heading", { name: "Media" })).toHaveClass(
      "text-base",
      "font-semibold",
    );
    expect(screen.getByText("Source file").parentElement).toHaveClass("pb-3");
  });

  it("adds no bottom padding without a footer, so a dock keeps its own edge", () => {
    render(
      <Panel title="Timeline">
        <p>Clip rows</p>
      </Panel>,
    );

    expect(screen.getByText("Clip rows").parentElement).not.toHaveClass("pb-3");
  });

  it("lets a caller drop the body padding with bodyClassName", () => {
    render(
      <Panel title="Timeline" bodyClassName="pb-0">
        <p>Canvas</p>
      </Panel>,
    );

    const body = screen.getByText("Canvas").parentElement;
    expect(body).toHaveClass("pb-0");
    expect(body).not.toHaveClass("pb-3");
  });
});

describe("EmptyState compact", () => {
  it("uses a smaller icon tile and a 14px title for short containers", () => {
    render(<EmptyState icon={FolderOpen} title="No clip on the timeline" compact />);

    const heading = screen.getByRole("heading", { name: "No clip on the timeline" });
    expect(heading).toHaveClass("text-sm");
    expect(heading.previousElementSibling).toHaveClass("h-8", "w-8");
  });

  it("keeps the 40px icon tile by default", () => {
    render(<EmptyState icon={FolderOpen} title="No clip on the timeline" />);

    const heading = screen.getByRole("heading", { name: "No clip on the timeline" });
    expect(heading).toHaveClass("text-base");
    expect(heading.previousElementSibling).toHaveClass("h-10", "w-10");
  });
});

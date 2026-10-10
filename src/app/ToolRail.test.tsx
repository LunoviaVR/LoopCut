import "@/ui/test/env";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import i18n from "@/i18n";
import { INSPECTOR_PANEL_ID, TOOLS, type ToolId, ToolRail, toolTabId } from "./ToolRail";

function Harness({ controlsId }: { controlsId?: string }) {
  const [tool, setTool] = useState<ToolId>("media");
  return (
    <>
      <ToolRail value={tool} onValueChange={setTool} controlsId={controlsId} />
      <output aria-label="Active tool">{tool}</output>
    </>
  );
}

describe("ToolRail keyboard navigation", () => {
  it("exposes one tab stop, on the active tool", () => {
    render(<Harness />);

    expect(screen.getByRole("tab", { name: "Media" })).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("tab", { name: "Trim" })).toHaveAttribute("tabindex", "-1");
    expect(screen.getByRole("tablist", { name: "Tools" })).toHaveAttribute(
      "aria-orientation",
      "vertical",
    );
  });

  it("moves focus and selects the next tool with ArrowDown", async () => {
    render(<Harness />);

    screen.getByRole("tab", { name: "Media" }).focus();
    await userEvent.keyboard("{ArrowDown}");

    expect(screen.getByRole("tab", { name: "Trim" })).toHaveFocus();
    expect(screen.getByRole("tab", { name: "Trim" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Active tool")).toHaveTextContent("trim");
  });

  it("wraps from the first tool to the last with ArrowUp", async () => {
    render(<Harness />);

    screen.getByRole("tab", { name: "Media" }).focus();
    await userEvent.keyboard("{ArrowUp}");

    expect(screen.getByRole("tab", { name: "Export" })).toHaveFocus();
    expect(screen.getByLabelText("Active tool")).toHaveTextContent("export");
  });

  it("jumps to the ends with Home and End", async () => {
    render(<Harness />);

    screen.getByRole("tab", { name: "Media" }).focus();
    await userEvent.keyboard("{End}");
    expect(screen.getByRole("tab", { name: "Export" })).toHaveFocus();

    await userEvent.keyboard("{Home}");
    expect(screen.getByRole("tab", { name: "Media" })).toHaveFocus();
  });

  it("selects a tool on click", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("tab", { name: "Color" }));

    expect(screen.getByRole("tab", { name: "Color" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Active tool")).toHaveTextContent("color");
  });

  it("lists every tool with its shortcut in the tooltip", async () => {
    render(<Harness />);

    screen.getByRole("tab", { name: "Captions" }).focus();
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 2000 });

    expect(tooltip).toHaveTextContent("Captions");
    expect(tooltip).toHaveTextContent("5");
    expect(TOOLS.map((tool) => tool.shortcut)).toEqual([
      "Ctrl+1",
      "Ctrl+2",
      "Ctrl+3",
      "Ctrl+4",
      "Ctrl+5",
      "Ctrl+6",
    ]);
  });
});

describe("ToolRail labels and tab pattern", () => {
  it("shows each tool's name under its icon, so the tools need no hover to tell apart", () => {
    render(<Harness />);

    for (const tool of TOOLS) {
      const name = i18n.t(tool.labelKey);
      expect(within(screen.getByRole("tab", { name })).getByText(name)).toBeInTheDocument();
    }
  });

  it("gives each tab an id, so the inspector can name itself by the active tab", () => {
    render(<Harness />);

    expect(screen.getByRole("tab", { name: "Crop" })).toHaveAttribute("id", toolTabId("crop"));
    expect(toolTabId("crop")).toBe("tool-tab-crop");
  });

  it("points each tab at the inspector panel while the panel is shown", () => {
    render(<Harness controlsId={INSPECTOR_PANEL_ID} />);

    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).toHaveAttribute("aria-controls", INSPECTOR_PANEL_ID);
    }
  });

  it("points at no panel while the inspector is collapsed", () => {
    render(<Harness />);

    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).not.toHaveAttribute("aria-controls");
    }
  });
});

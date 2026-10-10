import "@/ui/test/env";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { TopBar, type TopBarProps } from "./TopBar";

function renderBar(props: Partial<TopBarProps> = {}) {
  const onExport = vi.fn();
  render(
    <TopBar
      projectName={null}
      exportEnabled={false}
      inspectorOpen
      onToggleInspector={() => {}}
      onExport={onExport}
      {...props}
    />,
  );
  return { onExport };
}

/** Hovers the wrapper that keeps the hint while the button is disabled, and returns the hint. */
async function hoverExportHint() {
  const button = screen.getByRole("button", { name: "Export" });
  await userEvent.hover(button.parentElement as HTMLElement);
  return screen.findByRole("tooltip");
}

describe("Export button hint", () => {
  it("says a video must be opened while no clip is open", async () => {
    renderBar({ exportEnabled: false });

    const button = screen.getByRole("button", { name: "Export" });
    expect(button).toBeDisabled();
    expect(button).toHaveAccessibleDescription("Open a video to export");
    expect(await hoverExportHint()).toHaveTextContent("Open a video to export");
  });

  it("says what the button does once a clip is open", async () => {
    renderBar({ exportEnabled: true });

    const button = screen.getByRole("button", { name: "Export" });
    expect(button).toBeEnabled();
    expect(button).toHaveAccessibleDescription("Export now with the current settings");
    expect(await hoverExportHint()).toHaveTextContent("Export now with the current settings");
  });

  it("is disabled and says an export is running while one runs", async () => {
    renderBar({ exportEnabled: true, exporting: true });

    const button = screen.getByRole("button", { name: "Export" });
    expect(button).toBeDisabled();
    expect(button).toHaveAccessibleDescription("An export is running");
    expect(await hoverExportHint()).toHaveTextContent("An export is running");
  });

  it("starts the export when enabled", async () => {
    const { onExport } = renderBar({ exportEnabled: true });

    await userEvent.click(screen.getByRole("button", { name: "Export" }));

    expect(onExport).toHaveBeenCalledTimes(1);
  });
});

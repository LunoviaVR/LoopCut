import "@/ui/test/env";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEstimateStore } from "@/features/export/estimateSync";
import { resetMediaSession } from "@/features/import/session";
import { resetProxyStates } from "@/features/media/proxy";
import { resetSettingsStore } from "@/features/settings/settingsStore";
import { resetUpdateStore } from "@/features/updates/updateStore";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { Shell } from "./Shell";

/** Shell renders: the top bar is a direct child of the shell, so each shell render calls it once. */
const shellRenders = vi.hoisted(() => ({ count: 0 }));

vi.mock("./TopBar", async (importOriginal) => {
  const { createElement } = await import("react");
  const actual = await importOriginal<typeof import("./TopBar")>();
  return {
    ...actual,
    TopBar: (props: Parameters<typeof actual.TopBar>[0]) => {
      shellRenders.count += 1;
      return createElement(actual.TopBar, props);
    },
  };
});

// The project and media live in shared stores, so every case starts from an empty editor.
beforeEach(() => {
  resetEditorState();
  resetMediaSession();
  resetProxyStates();
  resetSettingsStore();
  resetUpdateStore();
  shellRenders.count = 0;
});

describe("Shell regions", () => {
  it("renders each editor region under its accessible name", () => {
    render(<Shell />);

    expect(screen.getByRole("banner")).toBeInTheDocument();
    expect(screen.getByRole("tablist", { name: "Tools" })).toBeInTheDocument();
    expect(screen.getByRole("main", { name: "Monitor" })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Timeline" })).toBeInTheDocument();
  });

  it("names the inspector as the tabpanel of the active tool's tab", async () => {
    render(<Shell />);

    expect(screen.getByRole("tabpanel", { name: "Media" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: "Crop" }));
    expect(screen.getByRole("tabpanel", { name: "Crop" })).toBeInTheDocument();
  });

  it("links every tool tab to the inspector only while the inspector is shown", async () => {
    render(<Shell />);
    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).toHaveAttribute("aria-controls", "inspector-panel");
    }

    await userEvent.click(screen.getByRole("button", { name: "Inspector" }));

    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).not.toHaveAttribute("aria-controls");
    }
  });

  it("sizes the timeline row to its tracks, so the monitor takes the spare height", () => {
    render(<Shell />);

    // The shell grid is the top bar's grandparent: the bar sits in its own grid cell.
    const grid = screen.getByRole("banner").parentElement?.parentElement as HTMLElement;
    // 124px of tracks, the 32px header that holds the title and the trim keys, and 16px of padding above and below.
    expect(grid.style.gridTemplateRows).toMatch(/172px$/);
  });

  it("shows the LoopCut wordmark and an untitled project before anything is open", () => {
    render(<Shell />);

    expect(within(screen.getByRole("banner")).getByText("LoopCut")).toBeInTheDocument();
    expect(within(screen.getByRole("banner")).getByText("Untitled project")).toBeInTheDocument();
  });

  it("disables Export until a clip is open", () => {
    render(<Shell />);

    expect(screen.getByRole("button", { name: "Export" })).toBeDisabled();
  });

  it("starts with the drop zone in the monitor", () => {
    render(<Shell />);

    const main = screen.getByRole("main", { name: "Monitor" });
    expect(
      within(main).getByRole("heading", { name: "Drop a video or press Ctrl+O" }),
    ).toBeInTheDocument();
    expect(within(main).getByRole("button", { name: "Open video" })).toBeInTheDocument();
  });

  it("collapses and restores the inspector from the top bar toggle", async () => {
    render(<Shell />);

    const toggle = screen.getByRole("button", { name: "Inspector" });
    expect(toggle).toHaveAttribute("aria-pressed", "true");

    await userEvent.click(toggle);
    expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();
    expect(toggle).toHaveAttribute("aria-pressed", "false");

    await userEvent.click(toggle);
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
  });

  it("opens the sample clip from the drop zone and fills the project surfaces", async () => {
    render(<Shell />);

    await userEvent.click(screen.getByRole("button", { name: "Open video" }));

    const inspector = await screen.findByRole("complementary", { name: "Inspector" });
    await waitFor(() => expect(within(inspector).getByText("ranked-ace.mp4")).toBeInTheDocument());
    expect(within(inspector).getByText("1920 × 1080")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
    expect(screen.getByRole("banner")).toHaveTextContent("ranked-ace");

    const main = screen.getByRole("main", { name: "Monitor" });
    expect(
      await within(main).findByRole("img", { name: "Preview of the GIF output" }),
    ).toBeInTheDocument();
    await waitFor(() => expect(main).toHaveTextContent("00:06.00"));
  });

  it("plays and pauses the preview from the transport", async () => {
    render(<Shell />);
    await userEvent.click(screen.getByRole("button", { name: "Open video" }));
    const main = screen.getByRole("main", { name: "Monitor" });
    const play = await within(main).findByRole("button", { name: "Play" });
    await waitFor(() => expect(play).toBeEnabled());

    await userEvent.click(play);
    expect(await within(main).findByRole("button", { name: "Pause" })).toBeInTheDocument();

    await userEvent.click(within(main).getByRole("button", { name: "Pause" }));
    expect(await within(main).findByRole("button", { name: "Play" })).toBeInTheDocument();
  });

  it("reopens a collapsed inspector when a tool is chosen", async () => {
    render(<Shell />);
    await userEvent.click(screen.getByRole("button", { name: "Inspector" }));
    expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("tab", { name: "Crop" }));

    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Crop" })).toHaveAttribute("aria-selected", "true");
  });
});

describe("Shell commands", () => {
  it("runs the registry shortcuts: Ctrl+3 selects crop and shows the inspector", async () => {
    render(<Shell />);
    await userEvent.click(screen.getByRole("button", { name: "Inspector" }));
    expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();

    await userEvent.keyboard("{Control>}3{/Control}");

    expect(screen.getByRole("tab", { name: "Crop" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(useEditorStore.getState().activeTool).toBe("crop");
  });

  it("opens settings with Ctrl+,", async () => {
    render(<Shell />);

    fireEvent.keyDown(window, { key: ",", code: "Comma", ctrlKey: true });

    expect(await screen.findByRole("dialog", { name: "Settings" })).toBeInTheDocument();
  });

  it("zooms the timeline from its buttons through the same commands as the shortcuts", async () => {
    render(<Shell />);
    await userEvent.click(screen.getByRole("button", { name: "Open video" }));
    const zoomIn = screen.getByRole("button", { name: "Zoom in timeline" });
    await waitFor(() => expect(zoomIn).toBeEnabled());

    await userEvent.click(zoomIn);
    expect(useEditorStore.getState().timelineZoom).toBeCloseTo(1.25);

    await userEvent.click(screen.getByRole("button", { name: "Fit timeline" }));
    expect(useEditorStore.getState().timelineZoom).toBe(1);
  });
});

describe("Shell renders", () => {
  it("does not re-render for a project edit, nor when the preview and the estimate follow it", async () => {
    render(<Shell />);
    await userEvent.click(screen.getByRole("button", { name: "Open video" }));
    const main = screen.getByRole("main", { name: "Monitor" });
    await waitFor(() => expect(main).toHaveTextContent("00:06.00"));
    await waitFor(() => expect(useEstimateStore.getState().estimate).not.toBeNull());
    const before = shellRenders.count;

    act(() => useEditorStore.getState().setSpeed(2));
    expect(shellRenders.count).toBe(before);

    // The estimate (after its debounce) and the preview report (after its own) land later, and the shell still holds.
    await waitFor(() => expect(useEstimateStore.getState().pending).toBe(false), { timeout: 3000 });
    await act(() => new Promise((resolve) => setTimeout(resolve, 100)));
    expect(shellRenders.count).toBe(before);
  });
});

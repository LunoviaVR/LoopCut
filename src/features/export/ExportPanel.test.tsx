import "@/ui/test/env";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSettingsStore } from "@/features/settings/settingsStore";
import { sampleCapabilities, sampleSettings } from "@/features/testFixtures";
import { exportEstimate } from "@/ipc/commands";
import { resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { ExportPanel } from "./ExportPanel";
import { resetEstimateState, useEstimateStore } from "./estimateSync";
import { resetExportJob, useExportStore } from "./exportStore";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    exportEstimate: vi.fn(async () => ({
      bytes: 3_565_000,
      low: 3_280_000,
      high: 3_850_000,
      frames: 120,
      basis: "sample" as const,
    })),
    exportPresets: vi.fn(async () => []),
  };
});

beforeEach(() => {
  resetEditorState();
  resetEstimateState();
  resetExportJob();
  useSettingsStore.setState({ capabilities: null, settings: null });
  useEditorStore.getState().loadProject(sampleProject());
});

/** The panel, subscribed to the store the way the inspector is. */
function Harness() {
  const project = useEditorStore((state) => state.project);
  if (!project) return null;
  return <ExportPanel project={project} />;
}

const exportSettings = () => useEditorStore.getState().project?.export;

describe("GIF settings", () => {
  it("shows the lossy and motion sliders for the Quality engine", () => {
    render(<Harness />);

    expect(screen.getByRole("textbox", { name: "Lossy" })).toHaveValue("100");
    expect(screen.getByRole("textbox", { name: "Motion" })).toHaveValue("100");
    expect(screen.queryByRole("textbox", { name: "Colors" })).not.toBeInTheDocument();
  });

  it("explains what the motion value trades away, as the lossy slider does", () => {
    render(<Harness />);

    expect(
      screen.getByText("100 keeps the most detail in moving areas. Lower values save size."),
    ).toBeInTheDocument();
  });

  it("switches to the Fast engine, which takes a palette size and a dither mode", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("radio", { name: "Fast" }));

    expect(exportSettings()?.gifEngine).toBe("fast");
    const colors = screen.getByRole("textbox", { name: "Colors" });
    expect(colors).toHaveValue("256");
    await userEvent.clear(colors);
    await userEvent.type(colors, "64{Enter}");
    expect(exportSettings()?.colors).toBe(64);
    expect(screen.queryByRole("textbox", { name: "Lossy" })).not.toBeInTheDocument();
  });

  it("types a quality and records it as one undo step", async () => {
    render(<Harness />);
    const quality = screen.getByRole("textbox", { name: "Quality" });

    await userEvent.clear(quality);
    await userEvent.type(quality, "70{Enter}");

    expect(exportSettings()?.quality).toBe(70);
    undo();
    expect(exportSettings()?.quality).toBe(90);
  });

  it("says the GIF work runs on the CPU", () => {
    render(<Harness />);
    expect(
      screen.getByText(
        "GIF palette and compression run on the CPU; the GPU speeds up decoding, scaling and effects.",
      ),
    ).toBeInTheDocument();
  });

  it("names the GPU the dithering runs on, as the hardware settings pick it", async () => {
    useSettingsStore.setState({ capabilities: sampleCapabilities(), settings: sampleSettings() });
    render(<Harness />);

    await userEvent.click(screen.getByRole("radio", { name: "GPU dither" }));

    expect(
      screen.getByText("Dithers on AMD Radeon RX 9060 XT. The GIF is still written on the CPU."),
    ).toBeInTheDocument();
  });

  it("says the dithering stays on the CPU when hardware acceleration is off", async () => {
    const settings = sampleSettings();
    useSettingsStore.setState({
      capabilities: sampleCapabilities(),
      settings: { ...settings, hw: { ...settings.hw, enabled: false } },
    });
    render(<Harness />);

    await userEvent.click(screen.getByRole("radio", { name: "GPU dither" }));

    expect(
      screen.getByText(
        "The hardware settings keep the dithering on the CPU. The GIF is still written on the CPU.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Dithers on/)).not.toBeInTheDocument();
  });

  it("tells the user to run the hardware test when no GPU is known yet", async () => {
    useSettingsStore.setState({ capabilities: null, settings: sampleSettings() });
    render(<Harness />);

    await userEvent.click(screen.getByRole("radio", { name: "GPU dither" }));

    expect(
      screen.getByText(
        "Run the hardware test in Settings to check GPU support. The GIF is still written on the CPU.",
      ),
    ).toBeInTheDocument();
  });
});

describe("target size", () => {
  it("turns on a target of 10 MB and the MB field changes it", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("switch", { name: "Limit file size" }));
    expect(exportSettings()?.targetBytes).toBe(10_000_000);

    const target = screen.getByRole("textbox", { name: "Target" });
    await userEvent.clear(target);
    await userEvent.type(target, "5{Enter}");
    expect(exportSettings()?.targetBytes).toBe(5_000_000);
  });

  it("turns the target off again", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("switch", { name: "Limit file size" }));

    await userEvent.click(screen.getByRole("switch", { name: "Limit file size" }));

    expect(exportSettings()?.targetBytes).toBeNull();
  });
});

describe("estimate", () => {
  it("says it is waiting for a setting change when no estimate exists yet", () => {
    render(<Harness />);
    expect(screen.getByText("Change a setting to estimate the size.")).toBeInTheDocument();
  });

  it("shows the size, its range and the fit against the limit", () => {
    useEstimateStore.setState({
      estimate: { bytes: 3_565_000, low: 3_280_000, high: 3_850_000, frames: 120, basis: "sample" },
      error: null,
      pending: false,
    });
    render(<Harness />);

    expect(screen.getByText("3.6 MB")).toBeInTheDocument();
    expect(screen.getByText("3.3 MB to 3.9 MB")).toBeInTheDocument();
    // The status badge says so once; the limit line appears only when a limit exists.
    expect(screen.getAllByText("No limit")).toHaveLength(1);
  });

  it("judges the estimate against a target with the same fit words as the inspector", () => {
    useEstimateStore.setState({
      estimate: { bytes: 3_565_000, low: 3_280_000, high: 3_850_000, frames: 120, basis: "sample" },
      error: null,
      pending: false,
    });
    useEditorStore.getState().setExport({ targetBytes: 1_000_000 });
    render(<Harness />);

    expect(screen.getByText("Over the limit")).toBeInTheDocument();
    expect(screen.getByText("Will lower quality to fit")).toBeInTheDocument();
  });

  it("says Fits, with no lowering note, when the estimate is under the target", () => {
    useEstimateStore.setState({
      estimate: { bytes: 3_565_000, low: 3_280_000, high: 3_850_000, frames: 120, basis: "sample" },
      error: null,
      pending: false,
    });
    useEditorStore.getState().setExport({ targetBytes: 10_000_000 });
    render(<Harness />);

    expect(screen.getByText("Fits")).toBeInTheDocument();
    expect(screen.queryByText("Will lower quality to fit")).not.toBeInTheDocument();
  });

  it("shows the reason the estimate failed as visible text, not as a tooltip", () => {
    useEstimateStore.setState({ estimate: null, error: "encoder missing", pending: false });
    render(<Harness />);

    expect(screen.getByRole("alert")).toHaveTextContent("Estimate unavailable");
    expect(screen.getByText("encoder missing")).toBeInTheDocument();
    expect(screen.getByText("Estimate unavailable")).not.toHaveAttribute("title");
  });

  it("asks for the estimate again with Try again", async () => {
    useEstimateStore.setState({ estimate: null, error: "encoder missing", pending: false });
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(screen.getByText("Estimating size")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("3.6 MB")).toBeInTheDocument());
    expect(exportEstimate).toHaveBeenCalled();
  });
});

describe("export action", () => {
  it("offers the export in the chosen format", () => {
    render(<Harness />);
    expect(screen.getByRole("button", { name: "Export GIF" })).toBeEnabled();
  });

  it("shows a failed export with its message and the detail the backend gave", () => {
    useExportStore.setState({
      status: "failed",
      error: "The GPU renderer failed during the export.",
      errorDetail: "device lost (DXGI_ERROR_DEVICE_REMOVED)",
    });
    render(<Harness />);

    expect(screen.getByRole("alert")).toHaveTextContent("The export did not finish.");
    expect(screen.getByText("The GPU renderer failed during the export.")).toBeInTheDocument();
    expect(screen.getByText("device lost (DXGI_ERROR_DEVICE_REMOVED)")).toBeInTheDocument();
  });
});

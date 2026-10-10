import "@/ui/test/env";
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import type { ExportFormat } from "@/ipc/bindings/ExportFormat";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { gifFpsForDisplay, Inspector, type InspectorProps } from "./Inspector";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    exportPresets: vi.fn(async () => [
      {
        id: "discord",
        name: "Discord",
        format: "gif",
        maxBytes: 10_000_000,
        maxWidth: null,
        maxHeight: null,
        maxFrames: null,
        maxFps: 50,
        recommendedWidth: 480,
        recommendedFps: 20,
        verified: false,
        checkedOn: "2026-10-10",
        source: null,
        note: "Conservative limit for free accounts.",
      },
    ]),
  };
});

beforeEach(() => {
  resetEditorState();
  useEditorStore.getState().loadProject(sampleProject());
});

/** The sample project with its export settings replaced, so the status line has a format and a target to show. */
function projectWithExport(patch: Partial<Project["export"]>): Project {
  const base = sampleProject();
  return { ...base, export: { ...base.export, ...patch } };
}

const estimate = {
  bytes: 3_565_000,
  low: 3_280_000,
  high: 3_850_000,
  frames: 120,
  basis: "sample" as const,
};

/** A preview geometry with only the output size, which is all the status line reads. */
const preview = { outputWidth: 480, outputHeight: 270 } as PreviewInfo;

/** A tool other than Export, so the status line is on screen. */
function renderInspector(overrides: Partial<InspectorProps> = {}) {
  const project = sampleProject();
  return render(
    <Inspector
      tool="color"
      media={sampleMedia()}
      project={project}
      preview={null}
      estimate={null}
      estimateError={null}
      {...overrides}
    />,
  );
}

describe("export status line", () => {
  it("names the format, the output size, the frame rate and the estimate in one line", () => {
    const project = projectWithExport({ format: "gif" as ExportFormat });
    renderInspector({ project, preview, estimate });

    expect(screen.getByText("GIF")).toBeInTheDocument();
    expect(screen.getByText("480 × 270")).toBeInTheDocument();
    expect(screen.getByText(`${gifFpsForDisplay(project.output.fps)} fps`)).toBeInTheDocument();
    expect(screen.getByText("3.6 MB")).toBeInTheDocument();
  });

  it("names a video format with its own name", () => {
    renderInspector({ project: projectWithExport({ format: "mp4H264" }) });
    expect(screen.getByText("MP4 (H.264)")).toBeInTheDocument();
  });

  it("has no size select: the size target is set only in the Export tool", () => {
    renderInspector({ project: projectWithExport({ targetBytes: 10_000_000 }), estimate });
    expect(screen.queryByRole("combobox", { name: "Size preset" })).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("hides itself on the Export tool, whose estimate section already shows the size", () => {
    renderInspector({ tool: "export" });
    expect(screen.queryByText("Estimating size")).not.toBeInTheDocument();
  });

  it("says the estimate is still running while none has arrived", () => {
    renderInspector();
    expect(screen.getByText("Estimating size")).toBeInTheDocument();
  });

  it("shows the fit against a target as the one badge, with the shared wording", () => {
    renderInspector({ estimate, project: projectWithExport({ targetBytes: 1_000_000 }) });
    expect(screen.getByText("Over the limit")).toBeInTheDocument();
    expect(screen.queryByText("Fits")).not.toBeInTheDocument();
  });

  it("shows Fits when even the high end of the estimate is under the target", () => {
    renderInspector({ estimate, project: projectWithExport({ targetBytes: 10_000_000 }) });
    expect(screen.getByText("Fits")).toBeInTheDocument();
  });

  it("judges the fit against the platform preset's upload limit when no target is set, as the Export tool does", async () => {
    renderInspector({
      estimate,
      project: projectWithExport({ preset: "discord", targetBytes: null }),
    });
    expect(await screen.findByText("Fits")).toBeInTheDocument();
  });

  it("shows no badge without a target", () => {
    renderInspector({ estimate, project: projectWithExport({ targetBytes: null }) });
    expect(screen.queryByText("Over the limit")).not.toBeInTheDocument();
    expect(screen.queryByText("Fits")).not.toBeInTheDocument();
    expect(screen.queryByText("Near the limit")).not.toBeInTheDocument();
  });

  it("says the estimate is unavailable as visible text when it failed", () => {
    renderInspector({ estimateError: "Encoder missing" });
    const unavailable = screen.getByText("Estimate unavailable");
    expect(unavailable).toBeInTheDocument();
    expect(unavailable).not.toHaveAttribute("title");
    expect(screen.queryByText("Estimating size")).not.toBeInTheDocument();
  });
});

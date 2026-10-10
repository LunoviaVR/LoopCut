import "@/ui/test/env";
import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { ColorCompareOverlay } from "./ColorCompareOverlay";
import { renderUngradedFrame } from "./compare";
import { resetCompareState, useCompareStore } from "./compareStore";

vi.mock("./compare", () => ({
  renderUngradedFrame: vi.fn(async () => null),
}));

const VIEWPORT = { width: 480, height: 270, dpr: 1 };

beforeEach(() => {
  resetEditorState();
  resetCompareState();
  vi.clearAllMocks();
  useEditorStore.getState().loadProject(sampleProject());
});

/** The overlay with the store's project, the way the monitor mounts it. */
function renderOverlay() {
  const project = useEditorStore.getState().project;
  if (!project) throw new Error("project");
  return render(<ColorCompareOverlay project={project} viewport={VIEWPORT} />);
}

describe("color compare overlay", () => {
  it("draws nothing and fetches no before frame while the comparison is off", () => {
    const { container } = renderOverlay();

    act(() => useEditorStore.getState().setPlayhead(2));

    expect(container).toBeEmptyDOMElement();
    expect(renderUngradedFrame).not.toHaveBeenCalled();
  });

  it("fetches the before frame for the playhead while paused, and again when it moves", () => {
    renderOverlay();

    act(() => useCompareStore.getState().setEnabled(true));
    expect(renderUngradedFrame).toHaveBeenCalledTimes(1);
    expect(renderUngradedFrame).toHaveBeenLastCalledWith(0, VIEWPORT);

    act(() => useEditorStore.getState().setPlayhead(5));
    expect(renderUngradedFrame).toHaveBeenCalledTimes(2);
    expect(renderUngradedFrame).toHaveBeenLastCalledWith(5, VIEWPORT);
    expect(screen.getByRole("slider", { name: "Before and after divider" })).toBeInTheDocument();
  });

  it("shows the pause hint instead of the split while playing, and stops following the playhead", () => {
    renderOverlay();
    act(() => useCompareStore.getState().setEnabled(true));
    vi.mocked(renderUngradedFrame).mockClear();

    act(() => useEditorStore.getState().setTransport({ isPlaying: true }));

    expect(screen.getByText("Pause to compare before and after.")).toBeInTheDocument();
    expect(screen.queryByRole("slider")).not.toBeInTheDocument();
    act(() => useEditorStore.getState().setPlayhead(7));
    expect(renderUngradedFrame).not.toHaveBeenCalled();
  });

  it("reads the pause hint in the foreground colour on a stronger scrim", () => {
    renderOverlay();
    act(() => useCompareStore.getState().setEnabled(true));
    act(() => useEditorStore.getState().setTransport({ isPlaying: true }));

    expect(screen.getByText("Pause to compare before and after.")).toHaveClass(
      "bg-app/80",
      "text-fg",
    );
  });

  it("gives the divider a 24px hit area around a 1px line", () => {
    renderOverlay();
    act(() => useCompareStore.getState().setEnabled(true));

    const divider = screen.getByRole("slider", { name: "Before and after divider" });
    expect(divider).toHaveClass("w-6");
    expect(divider.firstElementChild).toHaveClass("w-px");
  });
});

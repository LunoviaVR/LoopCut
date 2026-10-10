import "@/ui/test/env";
import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { TrimPanel } from "./TrimPanel";

beforeEach(() => {
  resetEditorState();
  useEditorStore.getState().loadProject(sampleProject());
});

/** Subscribes to the store the way the inspector does, so edits re-render the panel. */
function Harness() {
  const project = useEditorStore((state) => state.project);
  if (!project) return null;
  return <TrimPanel media={sampleMedia()} project={project} preview={null} />;
}

function renderTrim() {
  return render(<Harness />);
}

const current = () => useEditorStore.getState().project;

describe("TrimPanel speed", () => {
  it("sets the speed from a preset button in one step", async () => {
    renderTrim();

    await userEvent.click(screen.getByRole("button", { name: "2×" }));

    expect(current()?.timing.speed).toBe(2);
    expect(screen.getByRole("button", { name: "2×" })).toHaveAttribute("aria-pressed", "true");
    undo();
    expect(current()?.timing.speed).toBe(1);
  });

  it("types a speed and records it as one undo step", async () => {
    renderTrim();
    const speed = screen.getByRole("textbox", { name: "Speed" });

    await userEvent.clear(speed);
    await userEvent.type(speed, "0.5{Enter}");

    expect(current()?.timing.speed).toBe(0.5);
    undo();
    expect(current()?.timing.speed).toBe(1);
    expect(canUndo()).toBe(false);
  });

  it("refuses an out-of-range speed with a visible message and keeps the old value", async () => {
    renderTrim();
    const speed = screen.getByRole("textbox", { name: "Speed" });

    await userEvent.clear(speed);
    await userEvent.type(speed, "9{Enter}");

    expect(current()?.timing.speed).toBe(1);
    expect(screen.getByRole("alert")).toHaveTextContent("0.25");
  });
});

describe("TrimPanel direction and crossfade", () => {
  it("switches direction from the segmented control", async () => {
    renderTrim();

    await userEvent.click(screen.getByRole("radio", { name: "Ping-pong" }));

    expect(current()?.timing.direction).toBe("pingPong");
  });

  it("disables the crossfade for ping-pong, where it does not apply", async () => {
    renderTrim();
    expect(screen.getByRole("textbox", { name: "Crossfade" })).toBeEnabled();

    await userEvent.click(screen.getByRole("radio", { name: "Ping-pong" }));

    expect(screen.getByRole("textbox", { name: "Crossfade" })).toBeDisabled();
    expect(
      screen.getByText("Ping-pong already loops seamlessly, so it has no crossfade."),
    ).toBeInTheDocument();
  });

  it("caps the crossfade at half the output length", async () => {
    renderTrim();
    const crossfade = screen.getByRole("textbox", { name: "Crossfade" });

    await userEvent.clear(crossfade);
    await userEvent.type(crossfade, "5{Enter}");

    expect(current()?.timing.crossfade).toBe(0);
    expect(screen.getByRole("alert")).toHaveTextContent("3");
  });
});

describe("TrimPanel output", () => {
  it("sets the width from a preset and shows the height it implies", async () => {
    renderTrim();

    await userEvent.click(screen.getByRole("button", { name: "320" }));

    expect(current()?.output.size).toEqual({ mode: "width", width: 320 });
    expect(screen.getByText("180 px")).toBeInTheDocument();
  });

  it("types a width", async () => {
    renderTrim();
    const width = screen.getByRole("textbox", { name: "Width" });

    await userEvent.clear(width);
    await userEvent.type(width, "400{Enter}");

    expect(current()?.output.size).toEqual({ mode: "width", width: 400 });
  });

  it("picks a preset frame rate and shows the rounding hint for a rate GIF cannot store", async () => {
    renderTrim();
    const fps = screen.getByRole("combobox", { name: "Frame rate" });
    expect(fps).toHaveTextContent("20 fps");

    useEditorStore.getState().setOutputFps({ num: 24, den: 1 });

    expect(await screen.findByRole("note")).toHaveTextContent("GIF stores centiseconds");
    expect(screen.getByRole("textbox", { name: "Custom frame rate" })).toHaveValue("24.00");
  });
});

describe("TrimPanel loop count", () => {
  it("switches between forever, once and a number of plays", async () => {
    renderTrim();

    await userEvent.click(screen.getByRole("radio", { name: "Once" }));
    expect(current()?.output.loopCount).toBe(1);

    await userEvent.click(screen.getByRole("radio", { name: "Times" }));
    expect(current()?.output.loopCount).toBe(3);
    const plays = screen.getByRole("textbox", { name: "Plays" });
    await userEvent.clear(plays);
    await userEvent.type(plays, "5{Enter}");
    expect(current()?.output.loopCount).toBe(5);

    await userEvent.click(screen.getByRole("radio", { name: "Forever" }));
    expect(current()?.output.loopCount).toBe(0);
    expect(screen.getAllByRole("radio", { name: /Forever|Once|Times/ })).toHaveLength(3);
  });
});

describe("TrimPanel range", () => {
  it("shows the kept range first, with the timeline keys that set it", () => {
    renderTrim();

    expect(screen.getByRole("heading", { name: "Range" })).toBeInTheDocument();
    expect(screen.getByText("Start")).toBeInTheDocument();
    expect(screen.getByText("0.00 s")).toBeInTheDocument();
    expect(screen.getByText("End")).toBeInTheDocument();
    expect(screen.getByText("6.00 s")).toBeInTheDocument();
    expect(screen.getByText("Set it on the timeline with I and O.")).toBeInTheDocument();
  });

  it("follows the kept range when the clip's segments change", () => {
    renderTrim();
    const clip = sampleProject().clips[0];
    if (!clip) throw new Error("the sample project has a clip");

    act(() => {
      useEditorStore
        .getState()
        .loadProject(sampleProject({ clips: [{ ...clip, segments: [{ start: 1, end: 4.25 }] }] }));
    });

    expect(screen.getByText("1.00 s")).toBeInTheDocument();
    expect(screen.getByText("4.25 s")).toBeInTheDocument();
  });
});

describe("TrimPanel fallbacks", () => {
  it("shows the derived height when the preview has not reported one yet", () => {
    renderTrim();
    expect(screen.getByText("270 px")).toBeInTheDocument();
  });

  it("fires a change event through fireEvent without losing the value", () => {
    renderTrim();
    const speed = screen.getByRole("textbox", { name: "Speed" });
    fireEvent.change(speed, { target: { value: "2" } });
    expect(speed).toHaveValue("2");
  });
});

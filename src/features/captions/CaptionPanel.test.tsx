import "@/ui/test/env";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import { fontsList } from "@/ipc/commands";
import { resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { resetFontCache } from "./CaptionControls";
import { CaptionPanel } from "./CaptionPanel";

// The font list is wrapped so a test can make one request fail; otherwise it answers as the mock backend does.
vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return { ...actual, fontsList: vi.fn(actual.fontsList) };
});

beforeEach(() => {
  resetEditorState();
  resetFontCache();
  vi.mocked(fontsList).mockClear();
  useEditorStore.getState().loadProject(sampleProject());
});

/** The panel, subscribed to the store the way the inspector is, so edits re-render it. */
function Harness() {
  const project = useEditorStore((state) => state.project);
  if (!project) return null;
  return <CaptionPanel media={sampleMedia()} project={project} preview={null} />;
}

const captions = () => useEditorStore.getState().project?.captions ?? [];

describe("caption list", () => {
  it("says so when there are no captions yet", () => {
    render(<Harness />);
    expect(screen.getByText("No captions yet. Add one below.")).toBeInTheDocument();
  });

  it("selects a caption from the list and deletes one with its trash button", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));
    await userEvent.click(screen.getByRole("button", { name: "Bottom text" }));
    expect(captions()).toHaveLength(2);

    const list = screen.getByRole("list", { name: "Captions" });
    await userEvent.click(
      within(list).getAllByRole("button", { name: /Top text/ })[0] as HTMLElement,
    );
    expect(useEditorStore.getState().selection).toEqual({ kind: "caption", id: captions()[0]?.id });

    await userEvent.click(
      within(list).getAllByRole("button", { name: "Delete caption" })[0] as HTMLElement,
    );
    expect(captions()).toHaveLength(1);
  });
});

describe("adding captions", () => {
  it("adds a top caption at the top of the frame, with its default text, and selects it", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    const [caption] = captions();
    expect(captions()).toHaveLength(1);
    expect(caption?.y).toBe(0.1);
    expect(caption?.text).toBe("Top text");
    expect(screen.getByRole("textbox", { name: "Caption text" })).toHaveValue("Top text");
  });

  it("imports a SubRip file as captions in one step", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("button", { name: "Import SRT" }));

    await waitFor(() => expect(captions()).toHaveLength(2));
    undo();
    expect(captions()).toHaveLength(0);
  });
});

describe("font list", () => {
  it("forgets a failed font list, so the next font field asks the backend again", async () => {
    vi.mocked(fontsList).mockRejectedValueOnce(new Error("fonts unavailable"));
    const first = render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The font list could not be loaded: fonts unavailable",
    );
    first.unmount();

    render(<Harness />);
    await waitFor(() => expect(vi.mocked(fontsList)).toHaveBeenCalledTimes(2));
    expect(screen.queryByText(/could not be loaded/)).not.toBeInTheDocument();
  });

  it("keeps one font list for the session, so later fields do not ask again", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));
    await waitFor(() => expect(vi.mocked(fontsList)).toHaveBeenCalledTimes(1));

    render(<Harness />);

    expect(vi.mocked(fontsList)).toHaveBeenCalledTimes(1);
  });
});

describe("editing the selected caption", () => {
  it("edits the text", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Text" }));
    const text = screen.getByRole("textbox", { name: "Caption text" });

    await userEvent.clear(text);
    await userEvent.type(text, "Hello there");
    await userEvent.tab();

    expect(captions()[0]?.text).toBe("Hello there");
  });

  it("moves the caption with the placement choice", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    await userEvent.click(screen.getByRole("radio", { name: "Bottom" }));

    expect(captions()[0]?.y).toBe(0.88);
  });

  it("removes the selected caption from the editor", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    await userEvent.click(screen.getByRole("button", { name: "Remove caption" }));

    expect(captions()).toHaveLength(0);
  });

  it("speaks each caption slider with its value and unit", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    expect(screen.getByRole("slider", { name: "Size" })).toHaveAttribute(
      "aria-valuetext",
      expect.stringMatching(/^\d+ px$/),
    );
    expect(screen.getByRole("slider", { name: "Outline width" })).toHaveAttribute(
      "aria-valuetext",
      expect.stringMatching(/^\d+\.\d{3}$/),
    );
  });

  it("names each colour swatch by its row label and opens the picker from it", async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole("button", { name: "Top text" }));

    await userEvent.click(screen.getByRole("button", { name: "Fill" }));

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
  });
});

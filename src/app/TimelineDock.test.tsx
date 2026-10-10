import "@/ui/test/env";
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import { mediaImport } from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { TimelineDock } from "./TimelineDock";

let media: ImportedMedia;

beforeEach(async () => {
  resetEditorState();
  media = await mediaImport("C:\\Clips\\ranked-ace.mp4");
  const project = sampleProject();
  const clip = project.clips[0];
  if (!clip) throw new Error("clip");
  useEditorStore.getState().loadProject({ ...project, clips: [{ ...clip, path: media.path }] });
});

describe("TimelineDock trim hint", () => {
  it("names the trim keys as key caps in the header while a clip is open", () => {
    render(<TimelineDock media={media} />);

    const hint = screen.getByText(/Drag the handles/);
    expect(hint).toHaveTextContent(
      "Drag the handles, or press I and O to set in and out. S cuts at the playhead. Delete removes the selected range.",
    );
    expect(Array.from(hint.querySelectorAll("kbd"), (key) => key.textContent)).toEqual([
      "I",
      "O",
      "S",
      "Delete",
    ]);
  });

  it("keeps the heading named Timeline, because the hint is hidden from screen readers", () => {
    render(<TimelineDock media={media} />);

    expect(screen.getByRole("heading", { name: "Timeline" })).toBeInTheDocument();
    expect(screen.getByText(/Drag the handles/).closest('[aria-hidden="true"]')).not.toBeNull();
  });

  it("shows no hint while no clip is open", () => {
    render(<TimelineDock media={null} />);

    expect(screen.queryByText(/Drag the handles/)).toBeNull();
  });
});

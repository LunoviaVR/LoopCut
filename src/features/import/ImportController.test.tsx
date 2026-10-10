import "@/ui/test/env";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import {
  appOpenPathArg,
  CommandError,
  mediaImport,
  mediaProxyStart,
  mediaRelease,
  onOpenPath,
  projectNew,
  projectOpen,
} from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { resetToasts, Toaster } from "@/ui";
import { resetMediaSession, useMediaSession } from "./session";
import { type ImportController, useImportController } from "./useImportController";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    mediaImport: vi.fn(),
    mediaRelease: vi.fn(),
    mediaProxyStart: vi.fn(),
    projectNew: vi.fn(),
    projectOpen: vi.fn(),
    appOpenPathArg: vi.fn(),
    onOpenPath: vi.fn(),
    projectSave: vi.fn(),
    projectRecoveryClear: vi.fn(),
  };
});

const media = sampleMedia();

/** A promise the test settles by hand, to hold an import open for as long as a check needs. */
function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

let controller: ImportController | null = null;

/** Mounts the hook the way the shell does: the controller's layer, plus the toast host. */
function Harness() {
  controller = useImportController();
  return (
    <>
      {controller.layer}
      <Toaster />
    </>
  );
}

function current(): ImportController {
  if (!controller) throw new Error("Harness is not mounted");
  return controller;
}

beforeEach(() => {
  vi.resetAllMocks();
  resetEditorState();
  resetMediaSession();
  resetToasts();
  controller = null;
  vi.mocked(mediaRelease).mockResolvedValue(undefined);
  vi.mocked(mediaProxyStart).mockResolvedValue(undefined);
  vi.mocked(appOpenPathArg).mockResolvedValue(null);
  vi.mocked(onOpenPath).mockResolvedValue(() => {});
  vi.mocked(projectNew).mockResolvedValue(sampleProject({ name: "ranked-ace" }));
});

describe("opening a video", () => {
  it("imports it and loads a new project into the editor", async () => {
    vi.mocked(mediaImport).mockResolvedValue(media);
    render(<Harness />);

    act(() => current().openPath(media.path));

    await waitFor(() => expect(useEditorStore.getState().project?.name).toBe("ranked-ace"));
    expect(mediaImport).toHaveBeenCalledWith(media.path, undefined);
    expect(useEditorStore.getState().projectPath).toBeNull();
  });

  it("asks before importing a file on a network share, then retries with confirmation", async () => {
    vi.mocked(mediaImport)
      .mockRejectedValueOnce(new CommandError({ code: "network_path", message: "UNC path" }))
      .mockResolvedValueOnce(media);
    render(<Harness />);

    act(() => current().openPath("\\\\server\\share\\clip.mp4"));

    const dialog = await screen.findByRole("dialog", {
      name: "Open a file from a network share?",
    });
    expect(mediaImport).toHaveBeenCalledTimes(1);
    await userEvent.click(within(dialog).getByRole("button", { name: "Import anyway" }));

    await waitFor(() =>
      expect(mediaImport).toHaveBeenLastCalledWith("\\\\server\\share\\clip.mp4", true),
    );
    await waitFor(() => expect(useEditorStore.getState().project).not.toBeNull());
  });

  it("does not import when the network share confirmation is cancelled", async () => {
    vi.mocked(mediaImport).mockRejectedValue(
      new CommandError({ code: "network_path", message: "UNC path" }),
    );
    render(<Harness />);

    act(() => current().openPath("\\\\server\\share\\clip.mp4"));
    const dialog = await screen.findByRole("dialog", {
      name: "Open a file from a network share?",
    });
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

    expect(mediaImport).toHaveBeenCalledTimes(1);
    expect(useEditorStore.getState().project).toBeNull();
  });

  it("reports a failed import as an error toast with a way to choose another file", async () => {
    vi.mocked(mediaImport).mockRejectedValue(
      new CommandError({ code: "unsupported", message: "No video stream found" }),
    );
    render(<Harness />);

    act(() => current().openPath("C:\\Clips\\notes.mp4"));

    expect(await screen.findByText("Could not open this file")).toBeInTheDocument();
    expect(screen.getByText(/No video stream found/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Choose another file" })).toBeInTheDocument();
    expect(useEditorStore.getState().project).toBeNull();
  });

  it("refuses unsupported file types before calling the backend", async () => {
    render(<Harness />);

    act(() => current().openPath("C:\\Clips\\notes.txt"));

    expect(await screen.findByText("This file cannot be opened")).toBeInTheDocument();
    expect(mediaImport).not.toHaveBeenCalled();
  });

  it("says that a project file with no clip cannot be opened, and opens nothing", async () => {
    vi.mocked(projectOpen).mockResolvedValue({
      project: sampleProject({ clips: [] }),
      path: "C:\\Clips\\empty.loopcut",
      tokens: {},
      migrated: false,
      missingMedia: [],
      networkMedia: [],
    });
    render(<Harness />);

    act(() => current().openPath("C:\\Clips\\empty.loopcut"));

    expect(await screen.findByText(/This project has no clip\./)).toBeInTheDocument();
    expect(mediaImport).not.toHaveBeenCalled();
    expect(useEditorStore.getState().project).toBeNull();
  });
});

describe("opening progress", () => {
  it("names the file while it opens, and clears the name once the project lands", async () => {
    const landing = deferred<ImportedMedia>();
    vi.mocked(mediaImport).mockReturnValue(landing.promise);
    render(<Harness />);

    act(() => current().openPath(media.path));

    await waitFor(() => expect(useMediaSession.getState().opening).toBe(media.fileName));
    act(() => landing.resolve(media));
    await waitFor(() => expect(useEditorStore.getState().project?.name).toBe("ranked-ace"));
    expect(useMediaSession.getState().opening).toBeNull();
  });

  it("clears the name when the import fails", async () => {
    vi.mocked(mediaImport).mockRejectedValue(
      new CommandError({ code: "unsupported", message: "No video stream found" }),
    );
    render(<Harness />);

    act(() => current().openPath("C:\\Clips\\notes.mp4"));

    expect(await screen.findByText("Could not open this file")).toBeInTheDocument();
    expect(useMediaSession.getState().opening).toBeNull();
  });

  it("keeps the name of the newest open while an older open lands", async () => {
    const first = deferred<ImportedMedia>();
    const second = deferred<ImportedMedia>();
    vi.mocked(mediaImport).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    render(<Harness />);

    act(() => current().openPath("C:\\Clips\\first.mp4"));
    act(() => current().openPath("C:\\Clips\\second.mp4"));
    await waitFor(() => expect(useMediaSession.getState().opening).toBe("second.mp4"));

    act(() =>
      first.resolve(
        sampleMedia({ path: "C:\\Clips\\first.mp4", fileName: "first.mp4", token: "tok-first" }),
      ),
    );
    await waitFor(() => expect(mediaRelease).toHaveBeenCalledWith("tok-first"));
    expect(useMediaSession.getState().opening).toBe("second.mp4");

    act(() => second.resolve(media));
    await waitFor(() => expect(useMediaSession.getState().opening).toBeNull());
  });
});

describe("unsaved changes", () => {
  it("asks before replacing a project with unsaved changes, and discards on request", async () => {
    useEditorStore.getState().loadProject(sampleProject({ name: "Old reaction" }));
    useEditorStore.setState({ dirty: true });
    vi.mocked(mediaImport).mockResolvedValue(media);
    render(<Harness />);

    act(() => current().openPath(media.path));

    const dialog = await screen.findByRole("dialog", { name: "Open another file?" });
    expect(within(dialog).getByText(/Old reaction/)).toBeInTheDocument();
    expect(mediaImport).not.toHaveBeenCalled();

    await userEvent.click(within(dialog).getByRole("button", { name: "Discard changes" }));
    await waitFor(() => expect(useEditorStore.getState().project?.name).toBe("ranked-ace"));
  });

  it("keeps the current project when the replacement is cancelled", async () => {
    useEditorStore.getState().loadProject(sampleProject({ name: "Old reaction" }));
    useEditorStore.setState({ dirty: true });
    render(<Harness />);

    act(() => current().openPath(media.path));
    const dialog = await screen.findByRole("dialog", { name: "Open another file?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

    expect(mediaImport).not.toHaveBeenCalled();
    expect(useEditorStore.getState().project?.name).toBe("Old reaction");
  });
});

describe("drag and drop in the browser build", () => {
  it("shows the drop cue while files are dragged over the window and opens the dropped file", async () => {
    vi.mocked(mediaImport).mockResolvedValue(media);
    render(<Harness />);

    const enter = new Event("dragenter", { bubbles: true, cancelable: true });
    Object.defineProperty(enter, "dataTransfer", { value: { types: ["Files"], files: [] } });
    act(() => {
      window.dispatchEvent(enter);
    });
    expect(await screen.findByText("Drop to open")).toBeInTheDocument();

    const drop = new Event("drop", { bubbles: true, cancelable: true });
    Object.defineProperty(drop, "dataTransfer", {
      value: { types: ["Files"], files: [new File(["bytes"], "ranked-ace.mp4")] },
    });
    act(() => {
      window.dispatchEvent(drop);
    });

    await waitFor(() =>
      expect(mediaImport).toHaveBeenCalledWith(
        "C:\\Users\\Demo\\Videos\\ranked-ace.mp4",
        undefined,
      ),
    );
    expect(screen.queryByText("Drop to open")).not.toBeInTheDocument();
  });
});

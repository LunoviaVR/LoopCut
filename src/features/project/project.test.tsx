import "@/ui/test/env";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RecoverySnapshot } from "@/ipc/bindings/RecoverySnapshot";
import {
  projectAutosave,
  projectRecovery,
  projectRecoveryClear,
  projectSave,
} from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { Toaster } from "@/ui";
import { useRecoveryPrompt } from "./RecoveryDialog";
import { saveProject } from "./saveProject";
import { useProjectAutosave } from "./useProjectAutosave";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    projectRecovery: vi.fn(),
    projectRecoveryClear: vi.fn(),
    projectSave: vi.fn(),
    projectAutosave: vi.fn(),
  };
});

const snapshot: RecoverySnapshot = {
  project: sampleProject({ name: "Unsaved reel" }),
  savedAt: "2026-10-09T18:40:00Z",
  originalPath: null,
};

beforeEach(() => {
  vi.resetAllMocks();
  resetEditorState();
  vi.mocked(projectRecovery).mockResolvedValue(null);
  vi.mocked(projectRecoveryClear).mockResolvedValue(undefined);
  vi.mocked(projectSave).mockResolvedValue(undefined);
  vi.mocked(projectAutosave).mockResolvedValue(undefined);
});

function RecoveryHarness({ restore }: { restore: (value: RecoverySnapshot) => void }) {
  const dialog = useRecoveryPrompt(restore);
  return (
    <>
      {dialog}
      <Toaster />
    </>
  );
}

describe("recovery prompt", () => {
  it("offers a recovered project from the snapshot at startup", async () => {
    vi.mocked(projectRecovery).mockResolvedValue(snapshot);
    render(<RecoveryHarness restore={vi.fn()} />);

    const dialog = await screen.findByRole("dialog", { name: "Recover unsaved project?" });
    expect(within(dialog).getByText(/Unsaved reel/)).toBeInTheDocument();
  });

  it("shows nothing when there is no snapshot", async () => {
    render(<RecoveryHarness restore={vi.fn()} />);

    await waitFor(() => expect(projectRecovery).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("Discard clears the recovery copy and closes the prompt", async () => {
    vi.mocked(projectRecovery).mockResolvedValue(snapshot);
    const restore = vi.fn();
    render(<RecoveryHarness restore={restore} />);

    await userEvent.click(await screen.findByRole("button", { name: "Discard" }));

    expect(projectRecoveryClear).toHaveBeenCalledTimes(1);
    expect(restore).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("Recover hands the snapshot to the import flow", async () => {
    vi.mocked(projectRecovery).mockResolvedValue(snapshot);
    const restore = vi.fn();
    render(<RecoveryHarness restore={restore} />);

    await userEvent.click(await screen.findByRole("button", { name: "Recover" }));

    expect(restore).toHaveBeenCalledWith(snapshot);
    expect(projectRecoveryClear).not.toHaveBeenCalled();
  });
});

describe("saving the project", () => {
  it("writes the project to a .loopcut file and marks it clean", async () => {
    const project = sampleProject({ name: "Sample loop" });
    useEditorStore.getState().loadProject(project);
    useEditorStore.setState({ dirty: true });

    const outcome = await saveProject();

    expect(outcome).toBe("saved");
    expect(projectSave).toHaveBeenCalledWith("Sample loop.loopcut", project);
    expect(useEditorStore.getState().projectPath).toBe("Sample loop.loopcut");
    expect(useEditorStore.getState().dirty).toBe(false);
    expect(projectRecoveryClear).toHaveBeenCalledTimes(1);
  });

  it("keeps the project dirty when the write fails, and says why", async () => {
    vi.mocked(projectSave).mockRejectedValue(new Error("Access is denied"));
    useEditorStore.getState().loadProject(sampleProject());
    useEditorStore.setState({ dirty: true });
    render(<Toaster />);

    const outcome = await saveProject();

    expect(outcome).toBe("failed");
    expect(useEditorStore.getState().dirty).toBe(true);
    expect(await screen.findByText("The project could not be saved")).toBeInTheDocument();
    expect(screen.getByText(/Access is denied/)).toBeInTheDocument();
  });

  it("keeps the recovery copy and the dirty flag when an edit lands during the write", async () => {
    vi.mocked(projectSave).mockImplementationOnce(async () => {
      useEditorStore.getState().setSpeed(2);
    });
    useEditorStore.getState().loadProject(sampleProject());
    useEditorStore.setState({ dirty: true });

    const outcome = await saveProject();

    expect(outcome).toBe("saved");
    expect(useEditorStore.getState().dirty).toBe(true);
    expect(projectRecoveryClear).not.toHaveBeenCalled();
  });

  it("does nothing when no project is open", async () => {
    expect(await saveProject()).toBe("unchanged");
    expect(projectSave).not.toHaveBeenCalled();
  });
});

describe("autosave", () => {
  it("saves the recovery copy shortly after an edit to a dirty project", async () => {
    useEditorStore.getState().loadProject(sampleProject());
    render(<AutosaveHarness />);

    useEditorStore.getState().updateProject((draft) => {
      draft.name = "Edited";
    });

    await waitFor(() => expect(projectAutosave).toHaveBeenCalled(), { timeout: 3000 });
    expect(vi.mocked(projectAutosave).mock.lastCall?.[0].name).toBe("Edited");
  });
});

function AutosaveHarness() {
  useProjectAutosave();
  return null;
}

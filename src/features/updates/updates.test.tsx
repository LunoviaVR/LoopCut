import "@/ui/test/env";
import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { saveProject } from "@/features/project/saveProject";
import { resetSettingsStore, useSettingsStore } from "@/features/settings/settingsStore";
import { fakeUpdate, sampleSettings } from "@/features/testFixtures";
import { appPrepareUpdate, appResumeBackground, settingsGet, settingsSet } from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { resetToasts, Toaster } from "@/ui";
import { installAvailableUpdate } from "./installUpdate";
import { UpdateBanner } from "./UpdateBanner";
import { checkForUpdate, runUpdateCheck } from "./updateCheck";
import { resetUpdateStore, useUpdateStore } from "./updateStore";

vi.mock("@tauri-apps/api/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tauri-apps/api/core")>();
  return { ...actual, isTauri: vi.fn(() => true) };
});

vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn(async () => {}) }));
vi.mock("@/features/project/saveProject", () => ({ saveProject: vi.fn(async () => "unchanged") }));

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    settingsGet: vi.fn(),
    settingsSet: vi.fn(),
    appPrepareUpdate: vi.fn(),
    appResumeBackground: vi.fn(),
    projectSave: vi.fn(),
    projectRecoveryClear: vi.fn(),
  };
});

const BANNER_TITLE = "LoopCut 0.0.2 is available";

beforeEach(async () => {
  resetToasts();
  vi.resetAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(check).mockResolvedValue(null);
  vi.mocked(relaunch).mockResolvedValue(undefined);
  vi.mocked(appPrepareUpdate).mockResolvedValue(undefined);
  vi.mocked(appResumeBackground).mockResolvedValue(undefined);
  vi.mocked(settingsSet).mockResolvedValue(undefined);
  vi.mocked(settingsGet).mockResolvedValue(sampleSettings());
  resetSettingsStore();
  resetUpdateStore();
  resetEditorState();
  await useSettingsStore.getState().load();
});

/** Makes the mocked release feed offer `update` on the next check. */
function offer(update = fakeUpdate()) {
  vi.mocked(check).mockResolvedValue({
    version: update.version,
    currentVersion: update.currentVersion,
    body: update.body ?? undefined,
    date: undefined,
    downloadAndInstall: (onEvent: Parameters<typeof update.downloadAndInstall>[0]) =>
      update.downloadAndInstall(onEvent),
    close: update.close,
  } as unknown as Awaited<ReturnType<typeof check>>);
}

describe("update checks", () => {
  it("a version the user skipped never reaches the banner", async () => {
    useSettingsStore.setState({
      settings: {
        ...sampleSettings(),
        updates: { ...sampleSettings().updates, skippedVersion: "0.0.2" },
      },
    });
    offer();

    const result = await runUpdateCheck();

    expect(result.kind).toBe("available");
    expect(useUpdateStore.getState().available).toBeNull();
  });

  it("records the time of a successful check", async () => {
    offer();

    await runUpdateCheck();

    await waitFor(() =>
      expect(settingsSet).toHaveBeenCalledWith(
        expect.objectContaining({
          updates: expect.objectContaining({ lastCheck: expect.any(String) }),
        }),
      ),
    );
  });

  it("reports a failed check as an error result instead of throwing", async () => {
    vi.mocked(check).mockRejectedValue(new Error("Could not fetch a valid release JSON"));

    const result = await checkForUpdate();

    expect(result).toEqual({ kind: "error", message: "Could not fetch a valid release JSON" });
    expect(useUpdateStore.getState().available).toBeNull();
    expect(settingsSet).not.toHaveBeenCalled();
  });
});

describe("update banner", () => {
  it("appears for a new version and stays until the user chooses", async () => {
    offer();
    await runUpdateCheck();
    render(<UpdateBanner />);

    expect(screen.getByText(BANNER_TITLE)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Release notes" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Skip this version" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Install and restart" })).toBeInTheDocument();
  });

  it("Later hides the banner until the next check finds a release", async () => {
    offer();
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(screen.queryByText(BANNER_TITLE)).not.toBeInTheDocument();
    expect(settingsSet).not.toHaveBeenCalledWith(
      expect.objectContaining({
        updates: expect.objectContaining({ skippedVersion: "0.0.2" }),
      }),
    );

    await runUpdateCheck();
    expect(await screen.findByText(BANNER_TITLE)).toBeInTheDocument();
  });

  it("Skip this version saves the choice and hides the banner", async () => {
    offer();
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Skip this version" }));

    await waitFor(() =>
      expect(settingsSet).toHaveBeenCalledWith(
        expect.objectContaining({
          updates: expect.objectContaining({ skippedVersion: "0.0.2" }),
        }),
      ),
    );
    expect(screen.queryByText(BANNER_TITLE)).not.toBeInTheDocument();
  });

  it("shows release notes as plain text, never as markup", async () => {
    offer(fakeUpdate({ body: "<img src=x onerror=alert(1)>\nFaster previews." }));
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Release notes" }));
    const dialog = await screen.findByRole("dialog", { name: "LoopCut 0.0.2" });

    expect(within(dialog).getByText(/<img src=x onerror=alert\(1\)>/)).toBeInTheDocument();
    expect(dialog.querySelector("img")).toBeNull();
  });

  it("installs, prepares the app first, and restarts after the download", async () => {
    offer();
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Install and restart" }));

    await waitFor(() => expect(relaunch).toHaveBeenCalledTimes(1));
    expect(appPrepareUpdate).toHaveBeenCalledTimes(1);
    expect(vi.mocked(appPrepareUpdate).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(relaunch).mock.invocationCallOrder[0] ?? 0,
    );
  });

  it("reopens the backend when the download fails after the app was prepared", async () => {
    offer(
      fakeUpdate({
        downloadAndInstall: async () => {
          throw new Error("Connection reset");
        },
      }),
    );
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Install and restart" }));

    await waitFor(() => expect(useUpdateStore.getState().phase).toBe("failed"));
    expect(appResumeBackground).toHaveBeenCalledTimes(1);
    expect(vi.mocked(appResumeBackground).mock.invocationCallOrder[0]).toBeGreaterThan(
      vi.mocked(appPrepareUpdate).mock.invocationCallOrder[0] ?? 0,
    );
  });

  it("does not reopen the backend when the prepare step itself failed", async () => {
    vi.mocked(appPrepareUpdate).mockRejectedValue({ code: "busy", message: "Still running" });
    offer();
    await runUpdateCheck();
    render(<UpdateBanner />);

    await userEvent.click(screen.getByRole("button", { name: "Install and restart" }));

    await waitFor(() => expect(useUpdateStore.getState().phase).toBe("failed"));
    expect(appResumeBackground).not.toHaveBeenCalled();
  });

  it("refuses to install while an export is running", async () => {
    offer();
    await runUpdateCheck();
    useEditorStore.setState({ isExporting: true });
    render(
      <>
        <UpdateBanner />
        <Toaster />
      </>,
    );

    await act(async () => {
      await installAvailableUpdate();
    });

    expect(await screen.findByText("Install after the export")).toBeInTheDocument();
    expect(appPrepareUpdate).not.toHaveBeenCalled();
    expect(relaunch).not.toHaveBeenCalled();
  });

  it("runs one install for a double request and hands both callers the same promise", async () => {
    offer();
    await runUpdateCheck();

    const first = installAvailableUpdate();
    const second = installAvailableUpdate();
    expect(second).toBe(first);
    await first;

    expect(appPrepareUpdate).toHaveBeenCalledTimes(1);
    expect(relaunch).toHaveBeenCalledTimes(1);
  });

  it("enters the preparing phase before the first await, so the banner holds its buttons at once", async () => {
    offer();
    await runUpdateCheck();

    const pending = installAvailableUpdate();
    expect(useUpdateStore.getState().phase).toBe("preparing");
    await pending;
  });

  it("re-checks for an export that started while the save was running", async () => {
    offer();
    await runUpdateCheck();
    useEditorStore.setState({ project: sampleProject(), dirty: true });
    vi.mocked(saveProject).mockImplementationOnce(async () => {
      useEditorStore.setState({ isExporting: true });
      return "saved";
    });
    render(
      <>
        <UpdateBanner />
        <Toaster />
      </>,
    );

    await act(async () => {
      await installAvailableUpdate();
    });

    // The toast store outlives each test, so an earlier refusal may still be on screen.
    expect((await screen.findAllByText("Install after the export")).length).toBeGreaterThan(0);
    expect(appPrepareUpdate).not.toHaveBeenCalled();
    expect(relaunch).not.toHaveBeenCalled();
    expect(useUpdateStore.getState().phase).toBe("idle");
  });

  it("says the update is installed when only the restart fails", async () => {
    offer();
    await runUpdateCheck();
    vi.mocked(relaunch).mockRejectedValueOnce(new Error("relaunch refused"));
    render(
      <>
        <UpdateBanner />
        <Toaster />
      </>,
    );

    await act(async () => {
      await installAvailableUpdate();
    });

    expect(
      (await screen.findAllByText("Update installed. Restart LoopCut to finish.")).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText("The update did not install")).not.toBeInTheDocument();
    expect(useUpdateStore.getState().phase).toBe("failed");
  });
});

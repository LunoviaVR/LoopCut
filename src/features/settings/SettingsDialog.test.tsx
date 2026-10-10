import "@/ui/test/env";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleCapabilities, sampleSettings } from "@/features/testFixtures";
import {
  appInfo,
  cacheClear,
  cacheUsage,
  hwCapabilities,
  settingsGet,
  settingsSet,
} from "@/ipc/commands";
import { SettingsDialog } from "./SettingsDialog";
import { resetSettingsStore, useSettingsStore } from "./settingsStore";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    settingsGet: vi.fn(),
    settingsSet: vi.fn(),
    hwCapabilities: vi.fn(),
    appInfo: vi.fn(),
    cacheUsage: vi.fn(),
    cacheClear: vi.fn(),
    hwProbe: vi.fn(),
  };
});

beforeEach(async () => {
  vi.resetAllMocks();
  resetSettingsStore();
  vi.mocked(settingsGet).mockResolvedValue(sampleSettings());
  vi.mocked(settingsSet).mockResolvedValue(undefined);
  vi.mocked(hwCapabilities).mockResolvedValue(sampleCapabilities());
  vi.mocked(appInfo).mockResolvedValue({
    version: "0.0.1",
    ffmpegVersion: "7.1.1",
    logDir: "C:\\Logs",
    dataDir: "C:\\Data",
  });
  vi.mocked(cacheUsage).mockResolvedValue({ bytes: 412_000_000, files: 37, path: "C:\\Cache" });
  vi.mocked(cacheClear).mockResolvedValue(undefined);
  await useSettingsStore.getState().load();
});

function renderOpen() {
  return render(<SettingsDialog open onOpenChange={() => {}} />);
}

describe("SettingsDialog", () => {
  it("lists the five sections and opens on General", () => {
    renderOpen();

    const dialog = screen.getByRole("dialog", { name: "Settings" });
    for (const name of ["General", "Performance", "Updates", "Storage", "About"]) {
      expect(within(dialog).getByRole("tab", { name })).toBeInTheDocument();
    }
    expect(within(dialog).getByRole("tab", { name: "General" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("switches sections and shows the performance controls", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });

    await userEvent.click(within(dialog).getByRole("tab", { name: "Performance" }));

    expect(
      await within(dialog).findByRole("switch", { name: "Hardware acceleration" }),
    ).toBeChecked();
    expect(within(dialog).getByRole("button", { name: "Run hardware test" })).toBeInTheDocument();
    expect(
      within(dialog).getByText(
        "GIF palette and compression always run on the CPU; the GPU speeds up decoding, scaling and effects, and encodes MP4/WebM.",
      ),
    ).toBeInTheDocument();
    await waitFor(() => expect(within(dialog).getAllByRole("table")).toHaveLength(1));
  });

  it("saves the hardware acceleration switch as soon as it changes", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "Performance" }));

    await userEvent.click(
      await within(dialog).findByRole("switch", { name: "Hardware acceleration" }),
    );

    await waitFor(() =>
      expect(settingsSet).toHaveBeenCalledWith(
        expect.objectContaining({ hw: expect.objectContaining({ enabled: false }) }),
      ),
    );
    expect(useSettingsStore.getState().settings?.hw.enabled).toBe(false);
  });

  it("saves the startup update check when its switch changes", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "Updates" }));

    await userEvent.click(
      await within(dialog).findByRole("switch", { name: "Check for updates on startup" }),
    );

    await waitFor(() =>
      expect(settingsSet).toHaveBeenCalledWith(
        expect.objectContaining({
          updates: expect.objectContaining({ checkOnStartup: false }),
        }),
      ),
    );
  });

  it("clears the cache only after an inline confirmation", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "Storage" }));

    expect(await within(dialog).findByText(/37 files/)).toBeInTheDocument();
    await userEvent.click(await within(dialog).findByRole("button", { name: "Clear cache" }));
    expect(cacheClear).not.toHaveBeenCalled();
    expect(
      within(dialog).getByText(/Delete the cached proxies and thumbnails/),
    ).toBeInTheDocument();

    await userEvent.click(within(dialog).getByRole("button", { name: "Clear cache" }));
    await waitFor(() => expect(cacheClear).toHaveBeenCalledTimes(1));
  });

  it("shows the probe result per adapter, with a row for each codec", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "Performance" }));

    const tables = await within(dialog).findAllByRole("table");
    expect(tables).toHaveLength(1);
    const [gpu] = tables;
    expect(gpu).toBeDefined();
    if (!gpu) return;
    expect(within(gpu).getByRole("rowheader", { name: "H.264" })).toBeInTheDocument();
    expect(within(gpu).getByText("D3D11VA: OK")).toBeInTheDocument();
    expect(within(gpu).getAllByText("AMF: OK")).toHaveLength(2);
    expect(within(gpu).getByText("min 256×144")).toBeInTheDocument();
  });

  it("shows the repository link as plain text with a neutral underline", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "About" }));

    const link = await within(dialog).findByRole("link", {
      name: "https://github.com/LunoviaVR/LoopCut",
    });
    expect(link.className).toContain("text-fg");
    expect(link.className).toContain("decoration-fg-muted");
    expect(link.className).not.toContain("decoration-accent");
    expect(link.className).not.toContain("text-accent");
  });

  it("names the plain number fields through their row labels", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });

    expect(within(dialog).getByRole("heading", { name: "Projects" })).toBeInTheDocument();
    expect(
      within(dialog).getByRole("textbox", { name: "Recent projects to keep" }),
    ).toBeInTheDocument();

    await userEvent.click(within(dialog).getByRole("tab", { name: "Storage" }));

    expect(within(dialog).getByRole("heading", { name: "Limits" })).toBeInTheDocument();
    expect(
      await within(dialog).findByRole("textbox", { name: "Maximum cache size" }),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "Memory budget" })).toBeInTheDocument();
  });

  it("explains what the on state of hardware acceleration does", async () => {
    renderOpen();
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await userEvent.click(within(dialog).getByRole("tab", { name: "Performance" }));

    expect(
      await within(dialog).findByText(
        "On lets the GPU decode, encode and render effects where the last hardware test verified it. Off decodes and encodes on the CPU and renders effects in software.",
      ),
    ).toBeInTheDocument();
  });
});

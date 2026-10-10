import "@/ui/test/env";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { resetMediaSession, useMediaSession } from "@/features/import/session";
import { sampleMedia } from "@/features/testFixtures";
import type { Project } from "@/ipc/bindings/Project";
import {
  previewFrame,
  previewPlay,
  previewSetProject,
  previewStatus,
  previewStop,
} from "@/ipc/commands";
import type { PreviewStatus } from "@/ipc/types";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { Toaster } from "@/ui";
import { PreviewMonitor } from "./PreviewMonitor";

const backend = vi.hoisted(() => ({
  playback: null as null | { onFrame: (buffer: ArrayBuffer) => void; resolve: () => void },
  status: {
    decodeProgress: 1,
    failedRanges: 0,
    fallbacks: [],
    playing: false,
    lastError: null,
  } as PreviewStatus,
}));

/** The backend is replaced by a stub: previews report 120 frames, playback streams until stopped. */
vi.mock("@/ipc/commands", () => {
  const frame = (): ArrayBuffer => {
    const buffer = new ArrayBuffer(8 + 2 * 2 * 4);
    const view = new DataView(buffer);
    view.setUint32(0, 2, true);
    view.setUint32(4, 2, true);
    return buffer;
  };
  return {
    previewSetProject: vi.fn(async () => ({
      outputWidth: 480,
      outputHeight: 270,
      renderWidth: 480,
      renderHeight: 270,
      frames: 120,
      fps: { num: 20, den: 1 },
      quality: "full",
      delaysCs: [],
    })),
    previewFrame: vi.fn(async () => frame()),
    previewPlay: vi.fn(
      (_from: number, _loop: boolean, onFrame: (buffer: ArrayBuffer) => void) =>
        new Promise<void>((resolve) => {
          backend.playback = { onFrame, resolve };
        }),
    ),
    previewStop: vi.fn(async () => {
      backend.playback?.resolve();
      backend.playback = null;
    }),
    previewStatus: vi.fn(async () => backend.status),
  };
});

const frameBuffer = (): ArrayBuffer => {
  const buffer = new ArrayBuffer(8 + 2 * 2 * 4);
  const view = new DataView(buffer);
  view.setUint32(0, 2, true);
  view.setUint32(4, 2, true);
  return buffer;
};

function renderMonitor() {
  return render(
    <>
      <PreviewMonitor
        project={sampleProject()}
        media={sampleMedia()}
        onOpenVideo={() => undefined}
        onPreviewChange={() => undefined}
      />
      <Toaster />
    </>,
  );
}

beforeEach(() => {
  resetEditorState();
  resetMediaSession();
  backend.playback = null;
  backend.status = {
    decodeProgress: 1,
    failedRanges: 0,
    fallbacks: [],
    playing: false,
    lastError: null,
  };
  useEditorStore.getState().loadProject(sampleProject());
  vi.mocked(previewPlay).mockClear();
  vi.mocked(previewStop).mockClear();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("PreviewMonitor overlay", () => {
  it("shows the output size, the GIF frame rate and the frame count once the preview is ready", async () => {
    renderMonitor();

    expect(await screen.findByText("480 × 270")).toBeInTheDocument();
    expect(screen.getByText("20 fps")).toBeInTheDocument();
    expect(screen.getByText("Frame 1 of 120")).toBeInTheDocument();
    expect(vi.mocked(previewSetProject)).toHaveBeenCalled();
  });

  it("names the format once, in the leading chip, not again in the frame rate", async () => {
    renderMonitor();

    await screen.findByText("480 × 270");
    expect(screen.getByText("GIF")).toBeInTheDocument();
    expect(screen.queryByText(/fps GIF/)).not.toBeInTheDocument();
  });
});

describe("PreviewMonitor under StrictMode", () => {
  it("still requests and draws frames, though React runs every effect twice in development", async () => {
    vi.mocked(previewFrame).mockClear();
    render(
      <StrictMode>
        <PreviewMonitor
          project={sampleProject()}
          media={sampleMedia()}
          onOpenVideo={() => undefined}
          onPreviewChange={() => undefined}
        />
      </StrictMode>,
    );

    await waitFor(() => expect(vi.mocked(previewFrame)).toHaveBeenCalled());
    expect(await screen.findByText("Frame 1 of 120")).toBeInTheDocument();
  });
});

describe("PreviewMonitor playback", () => {
  it("streams from the playhead, advances it once per frame, and stops on pause", async () => {
    renderMonitor();
    const play = await screen.findByRole("button", { name: "Play" });
    await waitFor(() => expect(play).toBeEnabled());

    await userEvent.click(play);

    expect(useEditorStore.getState().isPlaying).toBe(true);
    expect(vi.mocked(previewPlay)).toHaveBeenCalledWith(0, true, expect.any(Function));
    expect(await screen.findByText("Reduced quality while playing")).toBeInTheDocument();

    act(() => {
      backend.playback?.onFrame(frameBuffer());
      backend.playback?.onFrame(frameBuffer());
      backend.playback?.onFrame(frameBuffer());
    });
    expect(useEditorStore.getState().playhead).toBe(2);

    await userEvent.click(await screen.findByRole("button", { name: "Pause" }));

    expect(useEditorStore.getState().isPlaying).toBe(false);
    expect(vi.mocked(previewStop)).toHaveBeenCalled();
    expect(useEditorStore.getState().playhead).toBe(2);
    expect(await screen.findByRole("button", { name: "Play" })).toBeInTheDocument();
  });

  it("toggles loop playback from the transport", async () => {
    renderMonitor();
    const loop = await screen.findByRole("button", { name: "Loop playback" });
    await waitFor(() => expect(loop).toBeEnabled());
    expect(loop).toHaveAttribute("aria-pressed", "true");

    await userEvent.click(loop);

    expect(useEditorStore.getState().loopPlayback).toBe(false);
    expect(screen.getByRole("button", { name: "Loop playback" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
  });

  it("does not start playback while the crop tool shows the uncropped frame", async () => {
    useEditorStore.getState().setActiveTool("crop");
    renderMonitor();
    const play = await screen.findByRole("button", { name: "Play" });

    expect(play).toBeDisabled();
    expect(useEditorStore.getState().isPlaying).toBe(false);
  });
});

describe("PreviewMonitor opening and failure", () => {
  it("offers Open video in the empty monitor when no file is opening", () => {
    render(
      <PreviewMonitor
        project={null}
        media={null}
        onOpenVideo={() => undefined}
        onPreviewChange={() => undefined}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Drop a video or press Ctrl+O" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open video" })).toBeEnabled();
  });

  it("names the file that is opening and holds Open video until it lands", () => {
    useMediaSession.setState({ opening: "ranked-ace.mp4" });
    render(
      <PreviewMonitor
        project={null}
        media={null}
        onOpenVideo={() => undefined}
        onPreviewChange={() => undefined}
      />,
    );

    expect(screen.getByRole("heading", { name: "Opening ranked-ace.mp4" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open video" })).toBeDisabled();
  });

  it("covers the open project with the name of a video that is opening", () => {
    useMediaSession.setState({ opening: "reaction.mp4" });
    renderMonitor();

    expect(screen.getByRole("status")).toHaveTextContent("Opening reaction.mp4");
  });

  it("says why the preview failed, and draws it again on request", async () => {
    vi.mocked(previewSetProject).mockRejectedValueOnce(
      new Error("The decoder could not open the clip."),
    );
    renderMonitor();

    expect(await screen.findByRole("alert")).toHaveTextContent("The preview could not be drawn");
    expect(screen.getByText("The decoder could not open the clip.")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("480 × 270")).toBeInTheDocument();
    expect(vi.mocked(previewSetProject).mock.calls.length).toBeGreaterThanOrEqual(2);
  });
});

describe("PreviewMonitor decoder status", () => {
  it("shows decode progress while the decoder warms up, then hides it", async () => {
    backend.status = {
      decodeProgress: 0.4,
      failedRanges: 0,
      fallbacks: [],
      playing: false,
      lastError: null,
    };
    renderMonitor();

    expect(await screen.findByRole("progressbar", { name: "Decoding source" })).toBeInTheDocument();

    backend.status = {
      decodeProgress: 1,
      failedRanges: 0,
      fallbacks: [],
      playing: false,
      lastError: null,
    };
    await waitFor(
      () =>
        expect(
          screen.queryByRole("progressbar", { name: "Decoding source" }),
        ).not.toBeInTheDocument(),
      { timeout: 2000 },
    );
  });

  it("keeps the decode progress it shows while the project is edited", async () => {
    backend.status = {
      decodeProgress: 0.4,
      failedRanges: 0,
      fallbacks: [],
      playing: false,
      lastError: null,
    };
    const project = sampleProject();
    const monitor = (next: Project) => (
      <>
        <PreviewMonitor
          project={next}
          media={sampleMedia()}
          onOpenVideo={() => undefined}
          onPreviewChange={() => undefined}
        />
        <Toaster />
      </>
    );
    const { rerender } = render(monitor(project));
    const bar = await screen.findByRole("progressbar", { name: "Decoding source" });
    await waitFor(() => expect(bar).toHaveAttribute("aria-valuenow", "40"));

    const edited = { ...project, grade: { ...project.grade, exposure: 1 } };
    rerender(monitor(edited));

    await waitFor(() =>
      expect(vi.mocked(previewSetProject)).toHaveBeenLastCalledWith(edited, expect.anything()),
    );
    expect(screen.getByRole("progressbar", { name: "Decoding source" })).toHaveAttribute(
      "aria-valuenow",
      "40",
    );
  });

  it("announces a hardware fallback once with a toast", async () => {
    backend.status = {
      decodeProgress: 1,
      failedRanges: 0,
      fallbacks: [{ from: "d3d11va", to: "cpu", reason: "device lost" }],
      playing: false,
      lastError: null,
    };
    renderMonitor();

    expect(
      await screen.findByText("Hardware decoding failed for this clip; using the CPU."),
    ).toBeInTheDocument();
    expect(vi.mocked(previewStatus)).toHaveBeenCalled();
  });
});

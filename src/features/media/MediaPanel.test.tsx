import "@/ui/test/env";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import type { ProxyEvent } from "@/ipc/bindings/ProxyEvent";
import { mediaProxyStart } from "@/ipc/commands";
import { MediaPanel } from "./MediaPanel";
import { resetProxyStates, startProxy } from "./proxy";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return { ...actual, mediaProxyStart: vi.fn() };
});

beforeEach(() => {
  vi.resetAllMocks();
  resetProxyStates();
});

describe("MediaPanel", () => {
  it("names the clip in the header with its duration and resolution", () => {
    render(<MediaPanel media={sampleMedia()} />);

    expect(screen.getByText("ranked-ace.mp4")).toBeInTheDocument();
    expect(screen.getByText("12.40 s · 1920 × 1080")).toBeInTheDocument();
  });

  it("shows the source facts of the file", () => {
    render(<MediaPanel media={sampleMedia()} />);

    expect(screen.getByText("H.264 (High)")).toBeInTheDocument();
    expect(screen.getByText("1920 × 1080")).toBeInTheDocument();
    expect(screen.getByText("30 fps")).toBeInTheDocument();
    expect(screen.getByText("21.8 MB")).toBeInTheDocument();
  });

  it("keeps the colour facts under Details, which opens on request", async () => {
    render(<MediaPanel media={sampleMedia()} />);

    expect(screen.queryByText("8-bit, yuv420p")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Details" }));

    expect(screen.getByText("8-bit, yuv420p")).toBeInTheDocument();
    expect(screen.getByText("BT.709, limited range")).toBeInTheDocument();
  });

  it("names the container and keeps FFmpeg's format list as the tooltip", () => {
    const media = sampleMedia();
    render(
      <MediaPanel
        media={sampleMedia({
          info: { ...media.info, container: "mov,mp4,m4a,3gp,3g2,mj2" },
        })}
      />,
    );

    expect(screen.getByText("MP4")).toHaveAttribute("title", "mov,mp4,m4a,3gp,3g2,mj2");
    expect(screen.queryByText("mov,mp4,m4a,3gp,3g2,mj2")).not.toBeInTheDocument();
  });

  it("marks variable frame rate, HDR and alpha with badges", async () => {
    const media = sampleMedia();
    render(
      <MediaPanel
        media={sampleMedia({
          info: {
            ...media.info,
            video: {
              ...media.info.video,
              variableFrameRate: true,
              hasAlpha: true,
              color: { ...media.info.video.color, transfer: "pq", wideGamut: true },
            },
          },
        })}
      />,
    );

    expect(screen.getByText("VFR")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByText("HDR PQ")).toBeInTheDocument();
    expect(screen.getByText("Alpha channel")).toBeInTheDocument();
    expect(screen.getByText("Wide gamut")).toBeInTheDocument();
  });

  it("shows the displayed size and the stored size for rotated video", () => {
    const media = sampleMedia();
    render(
      <MediaPanel
        media={sampleMedia({
          info: { ...media.info, video: { ...media.info.video, rotation: 90 } },
        })}
      />,
    );

    expect(screen.getByText("1080 × 1920")).toBeInTheDocument();
    expect(screen.getByText("Stored as 1920 × 1080, rotated 90°")).toBeInTheDocument();
  });

  it("reports the original file when no proxy is needed", () => {
    render(<MediaPanel media={sampleMedia({ needsProxy: false })} />);

    expect(screen.getByText("Original file")).toBeInTheDocument();
  });

  it("shows proxy progress while the proxy is generated, then that it is in use", async () => {
    const media = sampleMedia({ needsProxy: true, token: "tok-proxy" });
    let deliver: ((event: ProxyEvent) => void) | undefined;
    let finish: (() => void) | undefined;
    vi.mocked(mediaProxyStart).mockImplementation(
      (_token, onProgress) =>
        new Promise<void>((resolve) => {
          deliver = onProgress;
          finish = resolve;
        }),
    );

    render(<MediaPanel media={media} />);
    act(() => startProxy(media));

    act(() => deliver?.({ kind: "progress", value: 0.5 }));
    expect(await screen.findByText("Building a preview copy, 50%")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Preview copy progress" })).toHaveAttribute(
      "aria-valuenow",
      "50",
    );

    act(() => {
      deliver?.({ kind: "done", url: "loopcut-media://proxy/tok-proxy" });
      finish?.();
    });
    await waitFor(() => expect(screen.getByText("Proxy in use")).toBeInTheDocument());
  });

  it("shows a proxy failure with the reason it gave", async () => {
    const media = sampleMedia({ needsProxy: true, token: "tok-failed" });
    vi.mocked(mediaProxyStart).mockImplementation(async (_token, onProgress) => {
      onProgress({
        kind: "error",
        error: { code: "encode_failed", message: "Encoder refused the frame size" },
      });
    });

    render(<MediaPanel media={media} />);
    act(() => startProxy(media));

    const status = await screen.findByText("Proxy failed");
    expect(
      within(status.parentElement as HTMLElement).getByText("Encoder refused the frame size"),
    ).toBeInTheDocument();
  });
});

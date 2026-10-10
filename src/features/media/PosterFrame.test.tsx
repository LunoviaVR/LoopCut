import "@/ui/test/env";
import { render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { thumbnailCountFor, thumbnailHeightFor } from "@/features/timeline/thumbnails";
import { fetchThumbnail, mediaThumbnails } from "@/ipc/commands";
import { PosterFrame } from "./PosterFrame";

vi.mock("@/ipc/commands", () => ({
  mediaThumbnails: vi.fn(),
  fetchThumbnail: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(mediaThumbnails).mockReset();
  vi.mocked(fetchThumbnail).mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("PosterFrame", () => {
  it("asks for the filmstrip set the timeline uses, so the backend keeps the timeline's set", async () => {
    vi.mocked(mediaThumbnails).mockRejectedValue(new Error("no media"));

    render(<PosterFrame token="tok-1" duration={12.4} />);

    await waitFor(() =>
      expect(mediaThumbnails).toHaveBeenCalledWith(
        "tok-1",
        thumbnailCountFor(12.4),
        thumbnailHeightFor(1),
      ),
    );
  });

  it("draws the middle frame of the set once it arrives", async () => {
    // jsdom has no 2D canvas, so the context is a spy: the test reads what the poster draws.
    const context = { clearRect: vi.fn(), drawImage: vi.fn(), putImageData: vi.fn() };
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(() => context as never);
    vi.mocked(mediaThumbnails).mockResolvedValue({ width: 4, height: 2, times: [0, 6, 12] });
    vi.mocked(fetchThumbnail).mockResolvedValue({
      width: 4,
      height: 2,
      rgba: new Uint8ClampedArray(new ArrayBuffer(4 * 2 * 4)),
    });

    const { container } = render(<PosterFrame token="tok-1" duration={12.4} />);

    await waitFor(() => expect(fetchThumbnail).toHaveBeenCalledWith("tok-1", 2, 1));
    await waitFor(() => expect(container.querySelector("canvas")).not.toHaveClass("invisible"));
    // A 4 x 2 frame letterboxed into the 72 x 40 poster: full width, 36 px tall, 2 px from the top.
    expect(context.drawImage).toHaveBeenCalledWith(expect.anything(), 0, 2, 72, 36);
  });

  it("keeps the film icon when no frame can be read", async () => {
    vi.mocked(mediaThumbnails).mockRejectedValue(new Error("no media"));

    const { container } = render(<PosterFrame token="tok-1" duration={12.4} />);

    await waitFor(() => expect(mediaThumbnails).toHaveBeenCalled());
    expect(container.querySelector("canvas")).toHaveClass("invisible");
    expect(container.querySelector("svg")).toBeInTheDocument();
  });
});

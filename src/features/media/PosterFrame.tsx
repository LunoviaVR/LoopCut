import { Film } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { containSize } from "@/features/preview/layout";
import {
  decodeThumbnail,
  thumbnailCountFor,
  thumbnailHeightFor,
} from "@/features/timeline/thumbnails";
import { fetchThumbnail, mediaThumbnails } from "@/ipc/commands";
import type { ThumbnailPixels } from "@/ipc/types";
import { cn } from "@/lib/cn";

/** Poster size in CSS pixels. */
const POSTER_WIDTH = 72;
const POSTER_HEIGHT = 40;

/**
 * The clip's middle frame as a 72 x 40 poster. It is read from the filmstrip set the timeline uses: the same count
 * and height, because the backend keeps one set per height and a different count would replace the timeline's.
 * Until a frame is drawn, or when there is none, a film icon stands in.
 */
export function PosterFrame({ token, duration }: { token: string; duration: number }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [drawn, setDrawn] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const dpr = window.devicePixelRatio || 1;
    setDrawn(false);
    mediaThumbnails(token, thumbnailCountFor(duration), thumbnailHeightFor(dpr))
      .then(async (set) => {
        const index = Math.floor(set.times.length / 2);
        const pixels = await fetchThumbnail(token, set.height, index);
        return { pixels, image: await decodeThumbnail(pixels) };
      })
      .then(({ pixels, image }) => {
        const canvas = canvasRef.current;
        if (!cancelled && canvas) {
          drawPoster(canvas, image, pixels, dpr);
          setDrawn(true);
        }
        closeBitmap(image);
      })
      .catch(() => {
        // No poster is not an error worth showing: the icon stays.
      });
    return () => {
      cancelled = true;
    };
  }, [token, duration]);

  return (
    <div className="relative h-10 w-18 shrink-0 overflow-hidden rounded-control bg-preview">
      {drawn ? null : (
        <div className="flex h-full w-full items-center justify-center text-fg-muted">
          <Film size={16} strokeWidth={1.5} aria-hidden="true" />
        </div>
      )}
      <canvas
        ref={canvasRef}
        className={cn("absolute inset-0 h-full w-full", !drawn && "invisible")}
      />
    </div>
  );
}

/** Letterboxes the frame into the poster, keeping its aspect ratio, at the device pixel ratio. */
function drawPoster(
  canvas: HTMLCanvasElement,
  image: CanvasImageSource,
  pixels: ThumbnailPixels,
  dpr: number,
): void {
  canvas.width = Math.round(POSTER_WIDTH * dpr);
  canvas.height = Math.round(POSTER_HEIGHT * dpr);
  const context = canvas.getContext("2d");
  if (!context) return;
  const fit = containSize(pixels.width, pixels.height, canvas.width, canvas.height);
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(
    image,
    (canvas.width - fit.width) / 2,
    (canvas.height - fit.height) / 2,
    fit.width,
    fit.height,
  );
}

/** Releases a decoded ImageBitmap; canvases from the fallback decoder need no release. */
function closeBitmap(image: CanvasImageSource): void {
  if (typeof ImageBitmap !== "undefined" && image instanceof ImageBitmap) image.close();
}

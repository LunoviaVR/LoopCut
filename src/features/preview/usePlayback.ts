/**
 * Playback for the monitor. Normal speed forward or backward (rate 1) streams frames from the backend; the playhead
 * advances by one per frame received, so the store follows what is on screen. Other rates (the J and L shuttle)
 * step through frames on a timer, each through the latest-wins loader. Pausing, a new run or unmounting ends the
 * stream (`previewStop`), and a run that has been superseded never writes to the store.
 */
import { type RefObject, useEffect, useRef } from "react";
import { toFloat } from "@/domain/time";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import { previewFrame, previewPlay, previewStop } from "@/ipc/commands";
import { FrameRenderer } from "@/ipc/frames";
import { useEditorStore } from "@/state/editor";
import { LatestFrameLoader } from "./frameLoader";

/** One renderer per canvas: its 2D context is acquired once for the whole stream. */
const renderers = new WeakMap<HTMLCanvasElement, FrameRenderer>();

/** Draws one wire-format frame into the canvas and returns the frame's size. */
export function drawInto(
  canvas: HTMLCanvasElement,
  buffer: ArrayBuffer,
): { width: number; height: number } {
  let renderer = renderers.get(canvas);
  if (!renderer) {
    renderer = new FrameRenderer(canvas);
    renderers.set(canvas, renderer);
  }
  return renderer.draw(buffer);
}

export interface PlaybackOptions {
  info: PreviewInfo | null;
  canvasRef: RefObject<HTMLCanvasElement | null>;
  /** Called once when a stream fails. Must be a stable callback. */
  onFailure: (error: unknown) => void;
}

/** Runs the transport while `isPlaying` is set in the store. */
export function usePlayback({ info, canvasRef, onFailure }: PlaybackOptions): void {
  const isPlaying = useEditorStore((state) => state.isPlaying);
  const playRate = useEditorStore((state) => state.playRate);
  const loop = useEditorStore((state) => state.loopPlayback);
  const runToken = useRef(0);

  useEffect(() => {
    if (!isPlaying || !info) return;
    const run = ++runToken.current;
    const isCurrent = () => run === runToken.current;
    const frames = Math.max(1, info.frames);
    const start = useEditorStore.getState().playhead;
    const finish = () => {
      if (isCurrent()) useEditorStore.getState().setTransport({ isPlaying: false, playRate: 1 });
    };
    const fail = (error: unknown) => {
      if (isCurrent()) onFailure(error);
    };
    let stopSteps = () => {};

    if (playRate === 1) {
      let received = 0;
      previewPlay(start, loop, (buffer) => {
        if (!isCurrent()) return;
        const canvas = canvasRef.current;
        if (canvas) drawInto(canvas, buffer);
        const index = start + received;
        received += 1;
        useEditorStore.getState().setPlayhead(loop ? index % frames : Math.min(index, frames - 1));
      })
        .catch(fail)
        .finally(finish);
    } else {
      const direction = playRate < 0 ? -1 : 1;
      const intervalMs = 1000 / (Math.max(1, toFloat(info.fps)) * Math.abs(playRate));
      const loader = new LatestFrameLoader({
        fetch: (request) => previewFrame(request.frame, request.mode),
        onFrame: (buffer) => {
          const canvas = canvasRef.current;
          if (isCurrent() && canvas) drawInto(canvas, buffer);
        },
        onError: fail,
      });
      let index = start;
      const timer = window.setInterval(() => {
        index += direction;
        if (index < 0 || index >= frames) {
          if (!loop) {
            window.clearInterval(timer);
            finish();
            return;
          }
          index = direction > 0 ? 0 : frames - 1;
        }
        useEditorStore.getState().setPlayhead(index);
        loader.request({ frame: index, mode: "output" });
      }, intervalMs);
      stopSteps = () => {
        window.clearInterval(timer);
        loader.dispose();
      };
    }

    return () => {
      stopSteps();
      runToken.current += 1;
      void previewStop();
    };
  }, [isPlaying, info, loop, playRate, canvasRef, onFailure]);
}

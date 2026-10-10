/**
 * Latest-wins frame requests for the paused monitor and the crop tool. At most one `previewFrame` call is in flight;
 * while it runs, further requests collapse into the newest one, which goes out as soon as the answer comes back.
 * An answer for an older request never overwrites a newer frame, and `reset()` drops everything still pending, so a
 * project change cannot paint a frame from the old project.
 */
import type { PreviewMode } from "@/ipc/types";

export interface FrameRequest {
  frame: number;
  mode: PreviewMode;
  /** Source time for `mode: "source"` (the crop tool's uncropped frame). */
  sourceTime?: number;
  /** Device pixel bounds for `mode: "source"`. */
  maxWidth?: number;
  maxHeight?: number;
}

export interface LatestFrameLoaderOptions {
  fetch: (request: FrameRequest) => Promise<ArrayBuffer>;
  onFrame: (buffer: ArrayBuffer, request: FrameRequest) => void;
  onError?: (error: unknown) => void;
}

interface Pending {
  id: number;
  generation: number;
  request: FrameRequest;
}

export class LatestFrameLoader {
  private inFlight = false;
  private pending: Pending | null = null;
  private nextId = 0;
  private shownId = 0;
  private generation = 0;
  private disposed = false;

  constructor(private readonly options: LatestFrameLoaderOptions) {}

  /** Whether a request is currently waiting for the backend. */
  get busy(): boolean {
    return this.inFlight;
  }

  /** Asks for a frame. Replaces any request that has not been sent yet. */
  request(request: FrameRequest): void {
    if (this.disposed) return;
    this.nextId += 1;
    this.pending = { id: this.nextId, generation: this.generation, request };
    this.pump();
  }

  /** Forgets the pending request and ignores any answer still on its way. */
  reset(): void {
    this.generation += 1;
    this.pending = null;
  }

  /**
   * Stops the loader: later requests and answers are ignored until `start()`. An owner calls `start()` in its effect and
   * `dispose()` in the cleanup, because React StrictMode runs both effects twice in development.
   */
  dispose(): void {
    this.disposed = true;
    this.reset();
  }

  /** Re-opens a loader that was disposed. */
  start(): void {
    this.disposed = false;
  }

  private pump(): void {
    if (this.inFlight || this.disposed || !this.pending) return;
    const job = this.pending;
    this.pending = null;
    this.inFlight = true;
    this.options
      .fetch(job.request)
      .then(
        (buffer) => {
          if (this.disposed || job.generation !== this.generation) return;
          if (job.id < this.shownId) return;
          this.shownId = job.id;
          this.options.onFrame(buffer, job.request);
        },
        (error: unknown) => {
          if (this.disposed || job.generation !== this.generation) return;
          this.options.onError?.(error);
        },
      )
      .finally(() => {
        this.inFlight = false;
        this.pump();
      });
  }
}

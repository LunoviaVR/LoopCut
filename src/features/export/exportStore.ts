/**
 * The state of the export job the panel shows: its status, the frames and bytes so far, the target passes and the
 * outcome. The export flow (exportFlow.ts) is the only writer. It is UI state, not part of the project.
 */
import { create } from "zustand";
import type { ExportFormat } from "@/ipc/bindings/ExportFormat";

export type ExportStatus = "idle" | "running" | "cancelling" | "done" | "failed" | "cancelled";

export interface ExportPass {
  pass: number;
  settings: string;
  bytes: number;
}

export interface ExportResult {
  path: string;
  bytes: number;
  seconds: number;
  frames: number;
}

export interface ExportJobState {
  status: ExportStatus;
  /** The id `export_start` returned, or null until it has. */
  jobId: string | null;
  /** Set when Cancel was pressed before the job id arrived; the cancel is then sent as soon as it does. */
  cancelRequested: boolean;
  path: string | null;
  format: ExportFormat | null;
  framesDone: number;
  framesTotal: number;
  bytes: number;
  /** `Date.now()` when the job started, for elapsed time and the ETA. */
  startedAt: number | null;
  passes: ExportPass[];
  /** Hardware fallbacks reported during this job. */
  fallbacks: number;
  result: ExportResult | null;
  /** The failure message, or null. */
  error: string | null;
  /** The technical detail the backend gave with the failure (tool output, GPU error text), or null. */
  errorDetail: string | null;
}

export function initialExportJob(): ExportJobState {
  return {
    status: "idle",
    jobId: null,
    cancelRequested: false,
    path: null,
    format: null,
    framesDone: 0,
    framesTotal: 0,
    bytes: 0,
    startedAt: null,
    passes: [],
    fallbacks: 0,
    result: null,
    error: null,
    errorDetail: null,
  };
}

export const useExportStore = create<ExportJobState>()(() => initialExportJob());

/** Whether a job is running (including one being cancelled). */
export function isExportActive(): boolean {
  const { status } = useExportStore.getState();
  return status === "running" || status === "cancelling";
}

/** Restores the idle state. Intended for tests and for a new project. */
export function resetExportJob(): void {
  useExportStore.setState(initialExportJob(), true);
}

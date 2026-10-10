import { useEstimateSync } from "@/features/export/estimateSync";

/**
 * Keeps the export size estimate in step with the open project. It renders nothing: it is a leaf, so the shell that
 * mounts it does not re-render for each project edit.
 */
export function EstimateSync(): null {
  useEstimateSync();
  return null;
}

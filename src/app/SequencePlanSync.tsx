import { useSequencePlanSync } from "@/features/timeline/useSequencePlanSync";

/**
 * Keeps the store's sequence plan in step with the project. It renders nothing: it is a leaf, so the timeline dock
 * that mounts it does not re-render for each project edit.
 */
export function SequencePlanSync(): null {
  useSequencePlanSync();
  return null;
}

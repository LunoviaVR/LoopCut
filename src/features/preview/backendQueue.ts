/**
 * Serializes calls to the preview backend. The backend holds one project at a time, so a task that swaps the project
 * (the before/after render sets a grade-off copy, renders, then restores) must not interleave with another preview
 * request. Every preview call that can race with such a swap goes through `enqueuePreviewTask`.
 *
 * Tasks run one after another in the order they were queued. A task that fails rejects its own promise and the queue
 * moves on to the next task.
 */
let tail: Promise<unknown> = Promise.resolve();

export function enqueuePreviewTask<T>(task: () => Promise<T>): Promise<T> {
  const run = tail.then(task);
  tail = run.catch(() => undefined);
  return run;
}

/**
 * One continuous edit (a handle drag, a slider drag, a scrub of a crop box) as a single undo step. Wraps
 * `beginGesture` / `endGesture` so the owning component never leaves a gesture open: `end()` is idempotent and
 * the component calls it from its unmount cleanup.
 */
import { useEffect, useState } from "react";
import { beginGesture, endGesture } from "./editor";

export class EditGesture {
  private open = false;

  /** Whether a gesture is currently open. */
  get isOpen(): boolean {
    return this.open;
  }

  /** Opens the gesture if it is not open yet. Calling it again is harmless. */
  begin(): void {
    if (this.open) return;
    beginGesture();
    this.open = true;
  }

  /** Opens the gesture if needed and runs one live edit inside it. */
  update(apply: () => void): void {
    this.begin();
    apply();
  }

  /** Closes the gesture. A gesture that changed nothing records no undo step. */
  end(): void {
    if (!this.open) return;
    this.open = false;
    endGesture();
  }
}

export interface EditGestureHandle {
  /** One live value (a drag step, a typed keystroke). Opens the gesture on the first call. */
  change: (apply: () => void) => void;
  /** The final value of a gesture: applies it and closes the gesture, so a single action is one undo step. */
  commit: (apply: () => void) => void;
}

/**
 * Gesture pairing for a control that edits continuously. `change` runs on every live value and `commit` on the
 * final one; a drag or a typed value between them is one undo step. The gesture is closed on unmount.
 */
export function useEditGesture(): EditGestureHandle {
  const [gesture] = useState(() => new EditGesture());
  useEffect(() => () => gesture.end(), [gesture]);
  return {
    change: (apply) => gesture.update(apply),
    commit: (apply) => {
      gesture.update(apply);
      gesture.end();
    },
  };
}

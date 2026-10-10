/**
 * File drags over the window. In Tauri the webview reports native paths (enter, over, leave, drop); in the
 * browser build the HTML5 drag events are used and only file names are available, which is enough for the
 * synthetic backend.
 */
import { isTauri } from "@tauri-apps/api/core";

export interface FileDragHandlers {
  /** A drag carrying files entered the window. */
  onDragStart: () => void;
  /** The drag left the window, was cancelled, or ended with a drop. */
  onDragEnd: () => void;
  /** Files were dropped. Paths are native paths in Tauri and file names in the browser build. */
  onDrop: (paths: string[]) => void;
}

/** Browser builds have no file system paths; the mock backend only needs a name to show. */
function browserPathFor(name: string): string {
  return `C:\\Users\\Demo\\Videos\\${name}`;
}

function carriesFiles(event: DragEvent): boolean {
  return Array.from(event.dataTransfer?.types ?? []).includes("Files");
}

function subscribeHtml5Drag(handlers: FileDragHandlers): () => void {
  let depth = 0;

  const onEnter = (event: DragEvent) => {
    if (!carriesFiles(event)) return;
    depth += 1;
    if (depth === 1) handlers.onDragStart();
  };
  const onLeave = (event: DragEvent) => {
    if (!carriesFiles(event)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) handlers.onDragEnd();
  };
  const onOver = (event: DragEvent) => {
    if (!carriesFiles(event)) return;
    // Without preventDefault the browser refuses the drop.
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  };
  const onDrop = (event: DragEvent) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    depth = 0;
    handlers.onDragEnd();
    const files = Array.from(event.dataTransfer?.files ?? []);
    handlers.onDrop(files.map((file) => browserPathFor(file.name)));
  };

  window.addEventListener("dragenter", onEnter);
  window.addEventListener("dragleave", onLeave);
  window.addEventListener("dragover", onOver);
  window.addEventListener("drop", onDrop);
  return () => {
    window.removeEventListener("dragenter", onEnter);
    window.removeEventListener("dragleave", onLeave);
    window.removeEventListener("dragover", onOver);
    window.removeEventListener("drop", onDrop);
  };
}

/** Subscribes to file drags over the window. Resolves to the unsubscribe function. */
export async function subscribeFileDrag(handlers: FileDragHandlers): Promise<() => void> {
  if (!isTauri()) return subscribeHtml5Drag(handlers);
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === "enter") handlers.onDragStart();
    else if (payload.type === "leave") handlers.onDragEnd();
    else if (payload.type === "drop") {
      handlers.onDragEnd();
      handlers.onDrop(payload.paths);
    }
  });
}

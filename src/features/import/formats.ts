/** Video containers and codecs the open dialog offers. FFmpeg decides what it can actually read. */
export const VIDEO_EXTENSIONS: readonly string[] = [
  "mp4",
  "mov",
  "mkv",
  "webm",
  "avi",
  "m4v",
  "gif",
  "wmv",
  "flv",
  "ts",
  "mts",
  "m2ts",
  "mxf",
  "ogv",
  "3gp",
  "apng",
  "webp",
];

export const PROJECT_EXTENSION = "loopcut";

export type PathKind = "video" | "project" | "unsupported";

export function fileNameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Lower-case extension without the dot, or an empty string when there is none. */
export function extensionOf(path: string): string {
  const name = fileNameOf(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

export function pathKind(path: string): PathKind {
  const extension = extensionOf(path);
  if (extension === PROJECT_EXTENSION) return "project";
  return VIDEO_EXTENSIONS.includes(extension) ? "video" : "unsupported";
}

/** Adds `.loopcut` when a chosen save path lacks it (Windows save dialogs do not always add it). */
export function ensureProjectExtension(path: string): string {
  return extensionOf(path) === PROJECT_EXTENSION ? path : `${path}.${PROJECT_EXTENSION}`;
}

/** Default file name for a project: the project name without characters Windows rejects. */
export function suggestedProjectFileName(name: string): string {
  const safe = name.replace(/[<>:"/\\|?*]/g, "_").trim();
  return `${safe || "Untitled"}.${PROJECT_EXTENSION}`;
}
